use crate::error::{Error, Result};
use crate::format::{MAX_NOTE_BYTES, valid_id};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use tempfile::NamedTempFile;

#[derive(Clone, Debug)]
pub struct Vault {
    root: PathBuf,
}

#[derive(Debug)]
pub struct Guard {
    _file: File,
    root: PathBuf,
}

impl Drop for Guard {
    fn drop(&mut self) {
        // fork другого потока может кратко удерживать унаследованный descriptor.
        let _ = self._file.unlock();
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanKind {
    Init,
    Replace,
    Edit,
    Publish,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanState {
    Preparing,
    Editing,
    Prepared,
    TargetWritten,
    SourceRemoved,
    Restoring,
    Complete,
    Abandoned,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub version: u32,
    pub operation_id: String,
    pub kind: PlanKind,
    pub from: String,
    pub to: String,
    pub base_hash: String,
    pub new_hash: String,
    pub backup: String,
    pub work: String,
    pub state: PlanState,
    pub config: Option<String>,
    #[serde(default)]
    pub initial_note: Option<String>,
}

impl Plan {
    pub fn recovery_paths(&self) -> Vec<String> {
        vec![
            format!(".zetto/recovery/{}/plan.json", self.operation_id),
            self.backup.clone(),
            self.work.clone(),
        ]
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect()
    }
    pub fn unfinished(&self) -> bool {
        !matches!(self.state, PlanState::Complete | PlanState::Abandoned)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum RecoveryMode {
    Resume,
    Restore,
}

pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

impl Vault {
    pub fn open(path: &Path) -> Result<Self> {
        let root = path
            .canonicalize()
            .map_err(|e| Error::new("io", format!("{}: {e}", path.display())))?;
        if !root.is_dir() {
            return Err(Error::new(
                "invalid_config",
                "Корень vault должен быть каталогом",
            ));
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path(&self, relative: &str) -> Result<PathBuf> {
        let path = Path::new(relative);
        if relative.is_empty()
            || relative.contains('\\')
            || path
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(Error::new(
                "validation_failed",
                format!("Опасный относительный путь: {relative}"),
            ));
        }
        let mut full = self.root.clone();
        for part in path.components() {
            full.push(part);
            match fs::symlink_metadata(&full) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(Error::new(
                        "validation_failed",
                        format!("Символическая ссылка запрещена: {relative}"),
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(full)
    }

    pub fn read(&self, relative: &str) -> Result<Vec<u8>> {
        let path = self.path(relative)?;
        if !fs::symlink_metadata(&path)?.is_file() {
            return Err(Error::new(
                "validation_failed",
                format!("Нужен обычный файл: {relative}"),
            ));
        }
        let file = File::open(path)?;
        let mut bytes = Vec::new();
        file.take((MAX_NOTE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_NOTE_BYTES {
            return Err(Error::new(
                "validation_failed",
                "zetto/resource-limit: файл превышает 16 MiB",
            ));
        }
        Ok(bytes)
    }

    fn read_optional(&self, relative: &str) -> Result<Option<Vec<u8>>> {
        let path = self.path(relative)?;
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
            Ok(_) => self.read(relative).map(Some),
        }
    }

    fn ensure_dir(&self, relative: &str) -> Result<()> {
        let mut current = String::new();
        for part in Path::new(relative).components() {
            let Component::Normal(part) = part else {
                return Err(Error::new("validation_failed", "Опасный каталог"));
            };
            if !current.is_empty() {
                current.push('/');
            }
            current.push_str(
                part.to_str()
                    .ok_or_else(|| Error::new("validation_failed", "Путь не UTF-8"))?,
            );
            let path = self.path(&current)?;
            match fs::create_dir(&path) {
                Ok(()) => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
                    }
                    sync_dir(path.parent().unwrap())?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    pub fn lock(&self) -> Result<Guard> {
        self.ensure_dir(".zetto/locks")?;
        let path = self.path(".zetto/locks/writer.lock")?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Guard {
                _file: file,
                root: self.root.clone(),
            }),
            Err(TryLockError::WouldBlock) => Err(Error::new(
                "lock_busy",
                "Vault изменяется другим процессом; повторите команду",
            )),
            Err(TryLockError::Error(e)) => Err(e.into()),
        }
    }

    fn check_guard(&self, guard: &Guard) -> Result<()> {
        if guard.root != self.root {
            return Err(Error::new("validation_failed", "Блокировка другого vault"));
        }
        Ok(())
    }

    fn write_atomic(&self, relative: &str, bytes: &[u8], replace: bool) -> Result<()> {
        if bytes.len() > MAX_NOTE_BYTES {
            return Err(Error::new(
                "validation_failed",
                "zetto/resource-limit: слишком большой файл",
            ));
        }
        let path = self.path(relative)?;
        let parent = path.parent().unwrap();
        let mut temp = NamedTempFile::new_in(parent)?;
        temp.write_all(bytes)?;
        temp.as_file().sync_all()?;
        let result = if replace {
            temp.persist(&path)
        } else {
            temp.persist_noclobber(&path)
        };
        result.map_err(|e| {
            if e.error.kind() == std::io::ErrorKind::AlreadyExists {
                Error::new("target_exists", format!("Файл уже существует: {relative}"))
            } else {
                e.error.into()
            }
        })?;
        sync_dir(parent)
    }

    pub fn create(&self, guard: &Guard, relative: &str, bytes: &[u8]) -> Result<()> {
        self.check_guard(guard)?;
        self.ensure_available(&[relative], None)?;
        self.write_atomic(relative, bytes, false)
    }

    pub fn note_paths(&self) -> Result<Vec<String>> {
        let mut result = Vec::new();
        for directory in ["", "inbox"] {
            let path = if directory.is_empty() {
                self.root.clone()
            } else {
                self.path(directory)?
            };
            let entries = match fs::read_dir(path) {
                Ok(entries) => entries,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            for entry in entries {
                let entry = entry?;
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| Error::new("validation_failed", "Имя файла не UTF-8"))?;
                if name.ends_with(".md") {
                    result.push(if directory.is_empty() {
                        name
                    } else {
                        format!("{directory}/{name}")
                    });
                }
            }
        }
        result.sort();
        Ok(result)
    }

    fn make_plan(
        &self,
        kind: PlanKind,
        from: &str,
        to: &str,
        base: &[u8],
        work: &[u8],
        config: Option<String>,
    ) -> Result<Plan> {
        let operation_id = ulid::Ulid::new().to_string();
        let directory = format!(".zetto/recovery/{operation_id}");
        self.ensure_dir(&directory)?;
        let backup = format!("{directory}/backup.md");
        let proposed = format!("{directory}/work.md");
        let plan = Plan {
            version: 1,
            operation_id,
            kind,
            from: from.into(),
            to: to.into(),
            base_hash: hash(base),
            new_hash: hash(work),
            backup,
            work: proposed,
            state: PlanState::Preparing,
            config,
            initial_note: if kind == PlanKind::Init {
                Some(
                    std::str::from_utf8(work)
                        .map_err(|_| Error::new("validation_failed", "Root должен быть UTF-8"))?
                        .into(),
                )
            } else {
                None
            },
        };
        self.save_plan(&plan)?;
        checkpoint("intent_saved")?;
        self.prepare_files(plan, base, work)
    }

    fn prepare_files(&self, mut plan: Plan, base: &[u8], work: &[u8]) -> Result<Plan> {
        if hash(base) != plan.base_hash || hash(work) != plan.new_hash {
            return Err(Error::new(
                "conflict",
                "Подготовка не соответствует хешам плана",
            ));
        }
        for (path, bytes, point) in [
            (&plan.backup, base, "backup_written"),
            (&plan.work, work, "work_written"),
        ] {
            match self.read_optional(path)? {
                Some(existing) if hash(&existing) != hash(bytes) => {
                    return Err(Error::new(
                        "conflict",
                        format!("Recovery-файл отличается: {path}"),
                    ));
                }
                Some(_) => {}
                None => self.write_atomic(path, bytes, false)?,
            }
            checkpoint(point)?;
        }
        plan.state = if plan.kind == PlanKind::Edit {
            PlanState::Editing
        } else {
            PlanState::Prepared
        };
        self.save_plan(&plan)?;
        checkpoint("plan_saved")?;
        Ok(plan)
    }

    fn save_plan(&self, plan: &Plan) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(plan)
            .map_err(|e| Error::new("recovery_required", e.to_string()))?;
        self.write_atomic(
            &format!(".zetto/recovery/{}/plan.json", plan.operation_id),
            &bytes,
            true,
        )
    }

    fn validate_plan(&self, plan: &Plan, id: &str) -> Result<()> {
        let prefix = format!(".zetto/recovery/{id}");
        if plan.version != 1
            || !valid_id(id)
            || plan.operation_id != id
            || plan.work != format!("{prefix}/work.md")
            || plan.backup != format!("{prefix}/backup.md")
        {
            return Err(Error::new(
                "recovery_required",
                "Неподдерживаемый или повреждённый план восстановления",
            ));
        }
        for relative in [&plan.work, &plan.backup, &plan.to] {
            self.path(relative)?;
        }
        let note_path = |p: &str| {
            p.ends_with(".md")
                && (Path::new(p).components().count() == 1
                    || (p.starts_with("inbox/") && Path::new(p).components().count() == 2))
        };
        if !note_path(&plan.to) || (plan.kind != PlanKind::Init && !note_path(&plan.from)) {
            return Err(Error::new(
                "recovery_required",
                "План меняет неуправляемый путь",
            ));
        }
        if plan.kind != PlanKind::Init {
            self.path(&plan.from)?;
        }
        if plan.kind == PlanKind::Publish
            && (!plan.from.starts_with("inbox/") || plan.to.contains('/'))
        {
            return Err(Error::new("recovery_required", "Неверные пути публикации"));
        }
        if matches!(plan.kind, PlanKind::Edit | PlanKind::Replace) && plan.from != plan.to {
            return Err(Error::new(
                "recovery_required",
                "План редактирования не может переносить файл",
            ));
        }
        Ok(())
    }

    pub fn plan(&self, id: &str) -> Result<Plan> {
        if !valid_id(id) {
            return Err(Error::new("usage", "Нужен полный ID операции"));
        }
        let bytes = self.read(&format!(".zetto/recovery/{id}/plan.json"))?;
        let plan: Plan = serde_json::from_slice(&bytes)
            .map_err(|e| Error::new("recovery_required", format!("Повреждённый план {id}: {e}")))?;
        self.validate_plan(&plan, id)?;
        Ok(plan)
    }

    pub fn plans(&self) -> Result<Vec<Plan>> {
        let entries = match fs::read_dir(self.path(".zetto/recovery")?) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut result = Vec::new();
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_symlink() {
                return Err(Error::new(
                    "recovery_required",
                    "Символическая ссылка в recovery",
                ));
            }
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let id = entry
                .file_name()
                .into_string()
                .map_err(|_| Error::new("recovery_required", "Неверное имя recovery"))?;
            if !valid_id(&id) {
                return Err(Error::new(
                    "recovery_required",
                    "Неверный ID каталога recovery",
                ));
            }
            if self
                .path(&format!(".zetto/recovery/{id}/plan.json"))?
                .exists()
            {
                result.push(self.plan(&id)?);
            }
        }
        result.sort_by(|a, b| a.operation_id.cmp(&b.operation_id));
        Ok(result)
    }

    fn ensure_available(&self, paths: &[&str], except: Option<&str>) -> Result<()> {
        for plan in self.plans()? {
            if plan.unfinished()
                && plan.state != PlanState::Editing
                && except != Some(&plan.operation_id)
                && paths
                    .iter()
                    .any(|p| *p == plan.from || *p == plan.to || plan.kind == PlanKind::Init)
            {
                return Err(Error::new(
                    "recovery_required",
                    format!("Сначала восстановите операцию {}", plan.operation_id),
                )
                .with_recovery(plan.recovery_paths()));
            }
        }
        Ok(())
    }

    pub fn replace(
        &self,
        guard: &Guard,
        relative: &str,
        expected: &str,
        bytes: &[u8],
    ) -> Result<Plan> {
        self.check_guard(guard)?;
        self.ensure_available(&[relative], None)?;
        let base = self.read(relative)?;
        let plan = self.make_plan(PlanKind::Replace, relative, relative, &base, bytes, None)?;
        if hash(&base) != expected {
            let mut abandoned = plan.clone();
            abandoned.state = PlanState::Abandoned;
            self.save_plan(&abandoned)?;
            return Err(Error::new(
                "conflict",
                "Оригинал изменился; предложенный текст сохранён отдельно",
            )
            .with_recovery(plan.recovery_paths()));
        }
        let recovery = plan.recovery_paths();
        self.resume(plan).map_err(|e| e.with_recovery(recovery))
    }

    pub fn begin_edit(&self, guard: &Guard, relative: &str) -> Result<Plan> {
        self.check_guard(guard)?;
        self.ensure_available(&[relative], None)?;
        let base = self.read(relative)?;
        self.make_plan(PlanKind::Edit, relative, relative, &base, &base, None)
    }

    pub fn finish_edit(&self, guard: &Guard, id: &str, bytes: &[u8]) -> Result<Plan> {
        self.check_guard(guard)?;
        let mut plan = self.plan(id)?;
        if plan.kind != PlanKind::Edit || plan.state != PlanState::Editing {
            return Err(Error::new(
                "recovery_required",
                "Операция не является открытой рабочей копией",
            ));
        }
        self.ensure_available(&[&plan.from], Some(id))?;
        self.write_atomic(&plan.work, bytes, true)?;
        plan.new_hash = hash(bytes);
        plan.state = PlanState::Prepared;
        self.save_plan(&plan)?;
        let recovery = plan.recovery_paths();
        self.resume(plan).map_err(|e| e.with_recovery(recovery))
    }

    pub fn publish(&self, guard: &Guard, from: &str, to: &str, expected: &str) -> Result<Plan> {
        self.check_guard(guard)?;
        self.ensure_available(&[from, to], None)?;
        if self.read_optional(to)?.is_some() {
            return Err(Error::new(
                "target_exists",
                format!("Цель существует: {to}"),
            ));
        }
        let base = self.read(from)?;
        if hash(&base) != expected {
            return Err(Error::new("conflict", "Черновик изменился"));
        }
        let plan = self.make_plan(PlanKind::Publish, from, to, &base, &base, None)?;
        self.validate_plan(&plan, &plan.operation_id)?;
        let recovery = plan.recovery_paths();
        self.resume(plan).map_err(|e| e.with_recovery(recovery))
    }

    pub fn recover(&self, guard: &Guard, id: &str, mode: RecoveryMode) -> Result<Plan> {
        self.check_guard(guard)?;
        let plan = self.plan(id)?;
        let recovery = plan.recovery_paths();
        match mode {
            RecoveryMode::Resume => self.resume(plan),
            RecoveryMode::Restore => self.restore(plan),
        }
        .map_err(|e| e.with_recovery(recovery))
    }

    fn verified(&self, path: &str, expected: &str) -> Result<Vec<u8>> {
        let bytes = self.read(path)?;
        if hash(&bytes) != expected {
            return Err(Error::new("conflict", format!("Хеш не совпадает: {path}")));
        }
        Ok(bytes)
    }

    fn resume(&self, mut plan: Plan) -> Result<Plan> {
        if plan.state == PlanState::Restoring {
            return self.restore(plan);
        }
        if plan.state == PlanState::Complete {
            return Ok(plan);
        }
        if plan.state == PlanState::Abandoned {
            return Err(Error::new("recovery_required", "Операция отменена"));
        }
        if plan.state == PlanState::Preparing {
            let (base, work) = if plan.kind == PlanKind::Init {
                (
                    Vec::new(),
                    plan.initial_note
                        .as_ref()
                        .ok_or_else(|| {
                            Error::new("recovery_required", "Нет исходной root-заметки в плане")
                        })?
                        .as_bytes()
                        .to_vec(),
                )
            } else {
                let base = match self.read_optional(&plan.backup)? {
                    Some(bytes) => bytes,
                    None => self.verified(&plan.from, &plan.base_hash)?,
                };
                let work = match self.read_optional(&plan.work)? {
                    Some(bytes) => bytes,
                    None if plan.new_hash == plan.base_hash => base.clone(),
                    None => {
                        return Err(Error::new(
                            "recovery_required",
                            "Подготовка прервана до сохранения новой версии; restore оставит оригинал",
                        ));
                    }
                };
                (base, work)
            };
            plan = self.prepare_files(plan, &base, &work)?;
        }
        if plan.kind == PlanKind::Edit && plan.state == PlanState::Editing {
            let bytes = self.read(&plan.work)?;
            plan.new_hash = hash(&bytes);
            plan.state = PlanState::Prepared;
            self.save_plan(&plan)?;
        }
        let bytes = self.verified(&plan.work, &plan.new_hash)?;
        checkpoint("prepared")?;
        match plan.kind {
            PlanKind::Replace | PlanKind::Edit => {
                let current = self.read(&plan.from)?;
                if hash(&current) != plan.new_hash {
                    if hash(&current) != plan.base_hash {
                        return Err(Error::new(
                            "conflict",
                            "Оригинал изменился; обе версии сохранены",
                        ));
                    }
                    self.verified(&plan.backup, &plan.base_hash)?;
                    self.write_atomic(&plan.from, &bytes, true)?;
                }
                checkpoint("target_written")?;
            }
            PlanKind::Publish => {
                if let Some(current) = self.read_optional(&plan.to)? {
                    if hash(&current) != plan.new_hash {
                        return Err(Error::new(
                            "conflict",
                            "Цель публикации отличается от плана",
                        ));
                    }
                } else {
                    self.verified(&plan.from, &plan.base_hash)?;
                    self.write_atomic(&plan.to, &bytes, false)?;
                }
                plan.state = PlanState::TargetWritten;
                self.save_plan(&plan)?;
                checkpoint("target_written")?;
                if let Some(current) = self.read_optional(&plan.from)? {
                    if hash(&current) != plan.base_hash {
                        return Err(Error::new(
                            "conflict",
                            "Черновик изменён после создания цели; обе копии сохранены",
                        ));
                    }
                    fs::remove_file(self.path(&plan.from)?)?;
                    sync_dir(self.path(&plan.from)?.parent().unwrap())?;
                }
                plan.state = PlanState::SourceRemoved;
                self.save_plan(&plan)?;
                checkpoint("source_removed")?;
            }
            PlanKind::Init => {
                let config = plan
                    .config
                    .as_ref()
                    .ok_or_else(|| Error::new("recovery_required", "В init-плане нет config"))?;
                let current_config = self.read_optional(".zettorc")?;
                if current_config
                    .as_ref()
                    .is_some_and(|current| current != config.as_bytes())
                {
                    return Err(Error::new(
                        "conflict",
                        "Конфигурация отличается от плана init",
                    ));
                }
                if let Some(current) = self.read_optional(&plan.to)? {
                    if hash(&current) != plan.new_hash {
                        return Err(Error::new(
                            "conflict",
                            "Корневая заметка отличается от плана init",
                        ));
                    }
                } else {
                    self.write_atomic(&plan.to, &bytes, false)?;
                }
                checkpoint("target_written")?;
                if current_config.is_none() {
                    self.write_atomic(".zettorc", config.as_bytes(), false)?;
                }
                checkpoint("config_written")?;
            }
        }
        plan.state = PlanState::Complete;
        self.save_plan(&plan)?;
        Ok(plan)
    }

    fn restore(&self, mut plan: Plan) -> Result<Plan> {
        if plan.state == PlanState::Abandoned {
            return Ok(plan);
        }
        if plan.kind == PlanKind::Init {
            return Err(Error::new(
                "recovery_required",
                "Init можно продолжить; автоматического удаления vault нет",
            ));
        }
        if plan.state == PlanState::Preparing && self.read_optional(&plan.work)?.is_none() {
            self.verified(&plan.from, &plan.base_hash)?;
            plan.state = PlanState::Abandoned;
            self.save_plan(&plan)?;
            return Ok(plan);
        }
        let backup = self.verified(&plan.backup, &plan.base_hash)?;
        plan.state = PlanState::Restoring;
        self.save_plan(&plan)?;
        if plan.kind == PlanKind::Publish {
            if let Some(current) = self.read_optional(&plan.from)? {
                if hash(&current) != plan.base_hash {
                    return Err(Error::new(
                        "conflict",
                        "Изменённый исходник не будет затёрт",
                    ));
                }
            } else {
                self.write_atomic(&plan.from, &backup, false)?;
            }
            if let Some(current) = self.read_optional(&plan.to)? {
                if hash(&current) != plan.new_hash {
                    return Err(Error::new("conflict", "Изменённая цель не будет удалена"));
                }
                fs::remove_file(self.path(&plan.to)?)?;
                sync_dir(self.path(&plan.to)?.parent().unwrap())?;
            }
        } else {
            let current = self.read(&plan.from)?;
            if hash(&current) == plan.new_hash {
                self.write_atomic(&plan.from, &backup, true)?;
            } else if hash(&current) != plan.base_hash && plan.kind != PlanKind::Edit {
                return Err(Error::new(
                    "conflict",
                    "Изменённый оригинал не будет затёрт",
                ));
            }
        }
        plan.state = PlanState::Abandoned;
        self.save_plan(&plan)?;
        Ok(plan)
    }

    pub fn initialize(path: &Path, root_name: &str, bytes: &[u8], config: &str) -> Result<Self> {
        if !path.exists() {
            fs::create_dir_all(path)?;
        }
        let vault = Self::open(path)?;
        let pending_init = vault
            .plans()?
            .into_iter()
            .find(|p| p.kind == PlanKind::Init && p.unfinished());
        if pending_init.is_none() && !vault.bootstrap_empty()? {
            return Err(Error::new(
                "target_exists",
                "Init требует новый или пустой каталог",
            ));
        }
        let guard = vault.lock()?;
        if let Some(plan) = vault
            .plans()?
            .into_iter()
            .find(|p| p.kind == PlanKind::Init && p.unfinished())
        {
            vault.recover(&guard, &plan.operation_id, RecoveryMode::Resume)?;
            return Ok(vault);
        }
        if !vault.bootstrap_empty()? {
            return Err(Error::new(
                "target_exists",
                "Vault уже создан другим процессом",
            ));
        }
        for dir in ["inbox", "assets", ".zetto/recovery", ".zetto/locks"] {
            vault.ensure_dir(dir)?;
        }
        let plan = vault.make_plan(
            PlanKind::Init,
            "",
            root_name,
            b"",
            bytes,
            Some(config.into()),
        )?;
        vault.validate_plan(&plan, &plan.operation_id)?;
        vault.save_plan(&plan)?;
        let recovery = plan.recovery_paths();
        vault.resume(plan).map_err(|e| e.with_recovery(recovery))?;
        drop(guard);
        Ok(vault)
    }

    fn bootstrap_empty(&self) -> Result<bool> {
        for entry in fs::read_dir(self.root())? {
            let name = entry?
                .file_name()
                .into_string()
                .map_err(|_| Error::new("target_exists", "Неизвестное имя файла"))?;
            match name.as_str() {
                "inbox" | "assets" => {
                    if fs::read_dir(self.path(&name)?)?.next().is_some() {
                        return Ok(false);
                    }
                }
                ".zetto" => {
                    for entry in fs::read_dir(self.path(".zetto")?)? {
                        let part = entry?.file_name().into_string().map_err(|_| {
                            Error::new("target_exists", "Неизвестный служебный путь")
                        })?;
                        if part != "locks" && part != "recovery" {
                            return Ok(false);
                        }
                        for entry in fs::read_dir(self.path(&format!(".zetto/{part}"))?)? {
                            if part != "locks" || entry?.file_name() != "writer.lock" {
                                return Ok(false);
                            }
                        }
                    }
                }
                _ => return Ok(false),
            }
        }
        Ok(true)
    }
}

fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)?.sync_all().map_err(Into::into)
}

fn checkpoint(_step: &'static str) -> Result<()> {
    #[cfg(test)]
    if FAIL_AFTER.with(|point| point.get() == Some(_step)) {
        return Err(Error::new(
            "interrupted",
            format!("Прерывание после {_step}"),
        ));
    }
    Ok(())
}

#[cfg(test)]
thread_local! { static FAIL_AFTER: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) }; }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dropped_guard_unlocks_even_with_transient_inherited_descriptor() {
        let dir = tempfile::tempdir().unwrap();
        let vault =
            Vault::initialize(&dir.path().join("vault"), "root.md", b"root", "config").unwrap();
        let guard = vault.lock().unwrap();
        let _inherited = guard._file.try_clone().unwrap();
        drop(guard);
        assert!(vault.lock().is_ok());
    }

    #[test]
    fn init_resumes_before_payload_preparation_finishes() {
        for step in ["intent_saved", "backup_written", "work_written"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("vault");
            FAIL_AFTER.with(|p| p.set(Some(step)));
            let result = Vault::initialize(&path, "root.md", b"root", "config");
            FAIL_AFTER.with(|p| p.set(None));
            assert_eq!(result.unwrap_err().code, "interrupted");
            let vault = Vault::initialize(&path, "other.md", b"other", "other").unwrap();
            assert_eq!(vault.read("root.md").unwrap(), b"root");
            assert_eq!(vault.read(".zettorc").unwrap(), b"config");
        }
    }
    #[test]
    fn interrupted_writes_resume_from_actual_files() {
        for kind in [PlanKind::Replace, PlanKind::Publish, PlanKind::Init] {
            let steps: &[&str] = match kind {
                PlanKind::Publish => {
                    &["plan_saved", "prepared", "target_written", "source_removed"]
                }
                PlanKind::Init => &["plan_saved", "prepared", "target_written", "config_written"],
                _ => &["plan_saved", "prepared", "target_written"],
            };
            for &step in steps {
                let dir = tempfile::tempdir().unwrap();
                let root = dir.path().join("vault");
                if kind == PlanKind::Init {
                    FAIL_AFTER.with(|p| p.set(Some(step)));
                    assert_eq!(
                        Vault::initialize(&root, "root.md", b"root", "config")
                            .unwrap_err()
                            .code,
                        "interrupted"
                    );
                    FAIL_AFTER.with(|p| p.set(None));
                    let vault = Vault::initialize(&root, "other.md", b"other", "other").unwrap();
                    assert_eq!(vault.read("root.md").unwrap(), b"root");
                    assert_eq!(vault.read(".zettorc").unwrap(), b"config");
                } else {
                    let vault = Vault::initialize(&root, "root.md", b"root", "config").unwrap();
                    let guard = vault.lock().unwrap();
                    vault.create(&guard, "inbox/draft.md", b"old").unwrap();
                    FAIL_AFTER.with(|p| p.set(Some(step)));
                    let result = if kind == PlanKind::Publish {
                        vault.publish(&guard, "inbox/draft.md", "published.md", &hash(b"old"))
                    } else {
                        vault.replace(&guard, "inbox/draft.md", &hash(b"old"), b"new")
                    };
                    assert_eq!(result.unwrap_err().code, "interrupted");
                    FAIL_AFTER.with(|p| p.set(None));
                    let plan = vault
                        .plans()
                        .unwrap()
                        .into_iter()
                        .find(|p| p.kind == kind)
                        .unwrap();
                    assert_eq!(vault.read(&plan.backup).unwrap(), b"old");
                    vault
                        .recover(&guard, &plan.operation_id, RecoveryMode::Resume)
                        .unwrap();
                    vault
                        .recover(&guard, &plan.operation_id, RecoveryMode::Resume)
                        .unwrap();
                    let output = if kind == PlanKind::Publish {
                        "published.md"
                    } else {
                        "inbox/draft.md"
                    };
                    assert_eq!(
                        vault.read(output).unwrap(),
                        if kind == PlanKind::Publish {
                            b"old"
                        } else {
                            b"new"
                        }
                    );
                }
            }
        }
    }

    #[test]
    fn publication_does_not_delete_externally_changed_source() {
        let dir = tempfile::tempdir().unwrap();
        let vault =
            Vault::initialize(&dir.path().join("vault"), "root.md", b"root", "config").unwrap();
        let guard = vault.lock().unwrap();
        vault.create(&guard, "inbox/draft.md", b"old").unwrap();
        FAIL_AFTER.with(|p| p.set(Some("target_written")));
        assert!(
            vault
                .publish(&guard, "inbox/draft.md", "published.md", &hash(b"old"))
                .is_err()
        );
        FAIL_AFTER.with(|p| p.set(None));
        fs::write(vault.path("inbox/draft.md").unwrap(), b"external").unwrap();
        let plan = vault
            .plans()
            .unwrap()
            .into_iter()
            .find(|p| p.kind == PlanKind::Publish)
            .unwrap();
        assert_eq!(
            vault
                .recover(&guard, &plan.operation_id, RecoveryMode::Resume)
                .unwrap_err()
                .code,
            "conflict"
        );
        assert_eq!(vault.read("inbox/draft.md").unwrap(), b"external");
        assert_eq!(vault.read("published.md").unwrap(), b"old");
    }
}
