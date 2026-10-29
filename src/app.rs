use crate::config::VaultConfig;
use crate::editor;
use crate::error::{Error, Result};
use crate::format::{self, valid_id};
use crate::note::{Diagnostic, Note, Severity};
use crate::resolver::{self, ResolutionKind};
use crate::vault::{Guard, Plan, PlanKind, PlanState, RecoveryMode, Vault, hash};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Published,
    Inbox,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StageFilter {
    Published,
    Inbox,
    All,
}

#[derive(Clone, Debug)]
pub struct Record {
    pub path: String,
    pub raw: Option<String>,
    pub note: Note,
    pub stage: Stage,
}

pub fn filename_id(path: &str) -> Option<&str> {
    let name = Path::new(path).file_name()?.to_str()?.strip_suffix(".md")?;
    let prefix = name.get(..26)?;
    (valid_id(prefix) && (name.len() == 26 || name.as_bytes().get(26) == Some(&b'-')))
        .then_some(prefix)
}

impl Record {
    fn from_text(path: String, raw: String) -> Self {
        let mut note = format::parse(&raw);
        if let Some(prefix) = filename_id(&path) {
            let stem = Path::new(&path)
                .file_stem()
                .and_then(|p| p.to_str())
                .unwrap_or("");
            let tail = &stem[prefix.len()..];
            if !tail.is_empty()
                && (!tail.starts_with('-')
                    || tail.len() == 1
                    || tail.len() > 61
                    || !tail[1..]
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'))
            {
                note.diagnostics.push(Diagnostic::new(
                    "zetto/invalid-filename",
                    Severity::Error,
                    "Slug должен быть ASCII lowercase длиной до 60 символов",
                ));
            }
        }
        if note.id().is_some_and(|id| filename_id(&path) != Some(id)) {
            note.diagnostics.push(Diagnostic::new(
                "zetto/id-filename-mismatch",
                Severity::Error,
                "ID frontmatter не совпадает с именем файла",
            ));
        }
        if let (Some(id), Some(title)) = (note.id(), note.title()) {
            let name = Path::new(&path)
                .file_name()
                .and_then(|p| p.to_str())
                .unwrap_or("");
            if filename_id(&path) == Some(id)
                && name != format::filename(id, title)
                && note.fields.get("slug_locked") != Some(&Value::Bool(true))
            {
                note.diagnostics.push(Diagnostic::new(
                    "zetto/slug-mismatch",
                    Severity::Warning,
                    "Slug не соответствует title; исправление только явной операцией",
                ));
            }
        }
        for d in &mut note.diagnostics {
            d.path = Some(path.clone());
        }
        let stage = if path.starts_with("inbox/") {
            Stage::Inbox
        } else {
            Stage::Published
        };
        Self {
            path,
            raw: Some(raw),
            note,
            stage,
        }
    }
    pub fn id(&self) -> Option<&str> {
        self.note.id().filter(|id| {
            valid_id(id) && filename_id(&self.path) == Some(*id) && self.note.writable()
        })
    }
    pub fn claims_id(&self, id: &str) -> bool {
        self.note.id() == Some(id) || filename_id(&self.path) == Some(id)
    }
    pub fn data(&self) -> Value {
        json!({"id":self.id(),"path":self.path,"title":self.note.title(),"stage":self.stage,"tags":self.note.tags(),"aliases":self.note.aliases(),"diagnostics":self.note.diagnostics})
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Warning {
    pub code: String,
    pub message: String,
    pub path: Option<String>,
}
#[derive(Clone, Debug)]
pub struct Catalog {
    pub records: Vec<Record>,
    pub warnings: Vec<Warning>,
    pub complete: bool,
}

impl Catalog {
    pub fn find(&self, id: &str) -> Result<&Record> {
        if !valid_id(id) {
            return Err(Error::new("usage", "Нужен полный канонический ULID"));
        }
        let found: Vec<_> = self.records.iter().filter(|r| r.claims_id(id)).collect();
        match found.as_slice() {
            [] => Err(Error::new("not_found", format!("Заметка не найдена: {id}"))),
            [record] => Ok(record),
            _ => Err(Error::new(
                "duplicate_id",
                format!("ID {id} используется несколькими файлами"),
            )),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Outcome {
    pub status: String,
    pub saved: bool,
    pub data: Value,
    pub warnings: Vec<Warning>,
    pub diagnostics: Vec<Diagnostic>,
    pub recovery: Vec<String>,
    pub complete: bool,
    pub truncated: bool,
}
impl Outcome {
    pub fn ok(data: Value) -> Self {
        Self {
            status: "ok".into(),
            saved: false,
            data,
            warnings: Vec::new(),
            diagnostics: Vec::new(),
            recovery: Vec::new(),
            complete: true,
            truncated: false,
        }
    }
    fn note(record: &Record, saved: bool) -> Self {
        let mut out = Self::ok(json!({"id":record.note.id(),"path":record.path,"changed":saved}));
        out.saved = saved;
        out.diagnostics = record.note.diagnostics.clone();
        out
    }
}

#[derive(Clone, Debug)]
pub struct App {
    pub vault: Vault,
    pub config: VaultConfig,
}

impl App {
    pub fn open(path: &Path) -> Result<Self> {
        let vault = Vault::open(path)?;
        let config = VaultConfig::load(&vault)?;
        Ok(Self { vault, config })
    }
    pub fn init(path: &Path, title: &str) -> Result<Outcome> {
        if title.trim().is_empty() {
            return Err(Error::new("validation_failed", "Корню нужен заголовок"));
        }
        let id = ulid::Ulid::new().to_string();
        let name = format::filename(&id, title);
        let fields = json!({"id":id,"title":title,"created":now()})
            .as_object()
            .unwrap()
            .clone();
        let raw = format::render(&fields, "").map_err(|e| Error::new("validation_failed", e))?;
        let config = VaultConfig {
            config_version: 1,
            format_version: 1,
            root_id: id,
        };
        let text =
            toml::to_string(&config).map_err(|e| Error::new("invalid_config", e.to_string()))?;
        let pending = if path.try_exists()? {
            let existing = Vault::open(path)?;
            if existing.path(".zettorc")?.try_exists()? {
                VaultConfig::load(&existing)?.supported()?;
            }
            existing
                .plans()?
                .into_iter()
                .find(|p| p.kind == PlanKind::Init && p.unfinished())
        } else {
            None
        };
        let vault = if let Some(plan) = pending {
            Self::doctor_at(path, Some(&plan.operation_id), Some(RecoveryMode::Resume))?;
            Vault::open(path)?
        } else {
            Vault::initialize(path, &name, raw.as_bytes(), &text)?
        };
        let app = Self::open(vault.root())?;
        let catalog = app.scan()?;
        let record = catalog.find(&app.config.root_id)?;
        let mut out = Outcome::note(record, true);
        out.data["vault"] = json!(vault.root());
        Ok(out)
    }

    fn supported(&self) -> Result<VaultConfig> {
        let config = VaultConfig::load(&self.vault)?;
        config.supported()?;
        Ok(config)
    }

    pub fn scan(&self) -> Result<Catalog> {
        self.scan_with_config(&VaultConfig::load(&self.vault)?, true)
    }

    fn scan_with_config(&self, config: &VaultConfig, derive_links: bool) -> Result<Catalog> {
        let mut records = Vec::new();
        let mut complete = true;
        for path in self.vault.note_paths()? {
            match self.vault.read(&path).and_then(|b| {
                String::from_utf8(b).map_err(|_| Error::new("validation_failed", "Файл не UTF-8"))
            }) {
                Ok(raw) => records.push(Record::from_text(path, raw)),
                Err(error) => {
                    let mut d =
                        Diagnostic::new("zetto/file-read-error", Severity::Error, error.message);
                    d.path = Some(path.clone());
                    let stage = if path.starts_with("inbox/") {
                        Stage::Inbox
                    } else {
                        Stage::Published
                    };
                    records.push(Record {
                        path,
                        raw: None,
                        note: Note {
                            diagnostics: vec![d],
                            ..Note::default()
                        },
                        stage,
                    });
                    complete = false;
                }
            }
        }
        let mut claims = BTreeMap::<String, BTreeSet<usize>>::new();
        for (index, r) in records.iter().enumerate() {
            for id in [r.note.id(), filename_id(&r.path)]
                .into_iter()
                .flatten()
                .filter(|id| valid_id(id))
            {
                claims.entry(id.to_owned()).or_default().insert(index);
            }
        }
        for (id, indices) in claims {
            if indices.len() > 1 {
                for index in indices {
                    let r = &mut records[index];
                    let mut d = Diagnostic::new(
                        "zetto/duplicate-id",
                        Severity::Error,
                        format!("ULID {id} повторяется"),
                    );
                    d.path = Some(r.path.clone());
                    r.note.diagnostics.push(d);
                }
            }
        }
        let mut derived = vec![Vec::new(); records.len()];
        // ponytail: lookup по полному каталогу на ссылку; индекс заменит его в v0.2.
        for (index, r) in records.iter().enumerate().filter(|_| derive_links) {
            let mut connected = false;
            for link in &r.note.links {
                let resolution = resolver::resolve(&self.vault, &records, &r.path, link);
                if let Some(target) = resolution.target {
                    connected |= records[target].stage == Stage::Published
                        && records[target].note.id() != r.note.id();
                }
                if let Some(mut d) = resolution.diagnostic {
                    d.path = Some(r.path.clone());
                    derived[index].push(d);
                }
            }
            if r.id() == Some(config.root_id.as_str()) && r.stage == Stage::Published {
                derived[index].push(Diagnostic::new(
                    "zetto/root-exception",
                    Severity::Info,
                    "Корневая заметка исключена из требования исходящей связи",
                ));
            } else if r.stage == Stage::Published && r.id().is_some() && !connected {
                derived[index].push(Diagnostic::new(
                    "zetto/link-before-publish",
                    Severity::Warning,
                    "Нет связи на другую опубликованную заметку",
                ));
            }
        }
        for (r, diagnostics) in records.iter_mut().zip(derived) {
            for mut d in diagnostics {
                d.path = Some(r.path.clone());
                push_diagnostic(&mut r.note.diagnostics, d);
            }
            complete &= r.note.writable();
        }
        records.sort_by(|a, b| {
            a.note
                .id()
                .cmp(&b.note.id())
                .then_with(|| a.path.cmp(&b.path))
        });
        let mut warnings = Vec::new();
        match self.vault.plans() {
            Ok(plans) => {
                for p in plans.into_iter().filter(|p| p.unfinished()) {
                    warnings.push(Warning {
                        code: "recovery_available".into(),
                        message: format!(
                            "Рабочая копия/операция {}: {:?}",
                            p.operation_id, p.state
                        ),
                        path: Some(format!(".zetto/recovery/{}/plan.json", p.operation_id)),
                    });
                }
            }
            Err(e) => warnings.push(Warning {
                code: "recovery_required".into(),
                message: e.message,
                path: Some(".zetto/recovery".into()),
            }),
        }
        if records
            .iter()
            .filter(|r| r.id() == Some(config.root_id.as_str()) && r.stage == Stage::Published)
            .count()
            != 1
        {
            warnings.push(Warning {
                code: "root_invalid".into(),
                message: "Корневая заметка отсутствует, повреждена или неоднозначна".into(),
                path: Some(".zettorc".into()),
            });
        }
        Ok(Catalog {
            records,
            warnings,
            complete,
        })
    }

    pub fn raw(&self, id: &str) -> Result<Vec<u8>> {
        let catalog = self.scan()?;
        let record = catalog.find(id)?;
        self.vault.read(&record.path)
    }
    pub fn show(&self, id: &str) -> Result<Outcome> {
        let catalog = self.scan()?;
        let record = catalog.find(id)?;
        let mut out = Outcome::ok(record.data());
        out.data["body"] = json!(record.note.body);
        out.data["metadata"] = json!(record.note.fields);
        out.data["links"] = json!(record.note.links);
        out.diagnostics = record.note.diagnostics.clone();
        out.complete = record.raw.is_some();
        out.warnings = catalog.warnings;
        Ok(out)
    }
    pub fn list(
        &self,
        stage: StageFilter,
        tag: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Outcome> {
        if !(1..=10_000).contains(&limit) {
            return Err(Error::new("usage", "limit должен быть от 1 до 10000"));
        }
        let catalog = self.scan()?;
        let matching: Vec<_> = catalog
            .records
            .iter()
            .filter(|r| match stage {
                StageFilter::All => true,
                StageFilter::Inbox => r.stage == Stage::Inbox,
                StageFilter::Published => r.stage == Stage::Published,
            })
            .filter(|r| tag.is_none_or(|tag| r.note.tags().iter().any(|t| t == tag)))
            .collect();
        let items: Vec<_> = matching
            .iter()
            .skip(offset)
            .take(limit)
            .map(|r| r.data())
            .collect();
        let truncated = offset.saturating_add(items.len()) < matching.len();
        let mut out = Outcome::ok(
            json!({"items":items,"next_offset":truncated.then_some(offset.saturating_add(limit))}),
        );
        out.complete = catalog.complete && !truncated;
        out.truncated = truncated;
        out.warnings = catalog.warnings;
        out.diagnostics = catalog
            .records
            .iter()
            .flat_map(|r| r.note.diagnostics.clone())
            .collect();
        Ok(out)
    }
    pub fn capture(&self, title: &str, body: &str) -> Result<Outcome> {
        let guard = self.vault.lock()?;
        let config = self.supported()?;
        let id = ulid::Ulid::new().to_string();
        let path = format!("inbox/{}", format::filename(&id, title));
        let catalog = self.scan_with_config(&config, false)?;
        if catalog.records.iter().any(|r| r.claims_id(&id)) {
            return Err(Error::new("duplicate_id", "Созданный ULID уже существует"));
        }
        let fields = json!({"id":id,"title":title,"created":now()})
            .as_object()
            .unwrap()
            .clone();
        let raw = format::render(&fields, body).map_err(|e| Error::new("validation_failed", e))?;
        self.vault.create(&guard, &path, raw.as_bytes())?;
        Ok(Outcome::note(&Record::from_text(path, raw), true))
    }

    fn mutable<'a>(&self, catalog: &'a Catalog, id: &str) -> Result<&'a Record> {
        let record = catalog.find(id)?;
        if record.id() != Some(id) {
            let mut e = Error::new(
                "validation_failed",
                "Исправьте структурные ошибки заметки перед изменением",
            );
            e.diagnostics = record.note.diagnostics.clone();
            return Err(e);
        }
        Ok(record)
    }

    pub fn link(&self, source: &str, target: &str, display: Option<&str>) -> Result<Outcome> {
        if display.is_some_and(|d| d.contains(['\n', '\r']) || d.contains("]]")) {
            return Err(Error::new(
                "usage",
                "Display ссылки не может содержать перевод строки или ]]",
            ));
        }
        let guard = self.vault.lock()?;
        self.supported()?;
        let catalog = self.scan()?;
        self.link_record(&guard, &catalog, source, target, display)
    }

    fn link_record(
        &self,
        guard: &Guard,
        catalog: &Catalog,
        source: &str,
        target: &str,
        display: Option<&str>,
    ) -> Result<Outcome> {
        let original = self.mutable(catalog, source)?;
        self.mutable(catalog, target)?;
        if original
            .note
            .links
            .iter()
            .any(|l| l.target == target && l.kind == crate::note::LinkKind::Wiki)
        {
            return Ok(Outcome::note(original, false));
        }
        let raw = original.raw.as_ref().unwrap();
        let mut note = format::parse(raw);
        let newline = if note.body.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        if !note.body.is_empty() && !note.body.ends_with('\n') {
            note.body.push_str(newline);
        }
        note.body.push_str(newline);
        note.body.push_str(&match display {
            Some(d) => format!("[[{target}|{d}]]"),
            None => format!("[[{target}]]"),
        });
        note.body.push_str(newline);
        update_timestamp(&mut note);
        let next = format::render(&note.fields, &note.body)
            .map_err(|e| Error::new("validation_failed", e))?;
        let next_record = Record::from_text(original.path.clone(), next);
        if !next_record
            .note
            .links
            .iter()
            .any(|link| link.kind == crate::note::LinkKind::Wiki && link.target == target)
        {
            return Err(Error::new(
                "validation_failed",
                "Ссылка попадает в незакрытый блок кода или HTML; закройте его в редакторе",
            ));
        }
        let plan = self.vault.replace(
            guard,
            &original.path,
            &hash(raw.as_bytes()),
            next_record.raw.as_ref().unwrap().as_bytes(),
        )?;
        let mut out = Outcome::note(&next_record, true);
        out.recovery = plan.recovery_paths();
        if source == target {
            let mut d = Diagnostic::new(
                "zetto/self-link",
                Severity::Warning,
                "Ссылка на себя не выполняет требование публикации",
            );
            d.path = Some(original.path.clone());
            out.diagnostics.push(d);
        }
        Ok(out)
    }

    pub fn publish(&self, id: &str, link: Option<&str>) -> Result<Outcome> {
        let guard = self.vault.lock()?;
        let config = self.supported()?;
        let mut catalog = self.scan()?;
        let original = self.mutable(&catalog, id)?;
        if original.stage == Stage::Published {
            return Ok(Outcome::note(original, false));
        }
        if let Some(target) = link {
            self.link_record(&guard, &catalog, id, target, None)
                .map_err(|mut e| {
                    e.saved = original.raw.is_some();
                    e.data = Some(Box::new(json!({"id":id,"path":original.path})));
                    e
                })?;
            catalog = self.scan()?;
        }
        let original = self.mutable(&catalog, id)?;
        let raw = original.raw.as_ref().unwrap();
        let record = Record::from_text(original.path.clone(), raw.clone());
        let target = format::filename(id, record.note.title().unwrap_or(""));
        let diagnostics = publish_diagnostics(
            &self.vault,
            &catalog.records,
            &record,
            &target,
            &config.root_id,
        );
        if diagnostics.iter().any(|d| d.severity == Severity::Error) {
            let mut error = Error::new(
                "publish_rejected",
                "Черновик сохранён; условия публикации не выполнены",
            );
            error.saved = true;
            error.data = Some(Box::new(json!({"id":id,"path":record.path})));
            error.diagnostics = diagnostics;
            return Err(error);
        }
        let plan = self
            .vault
            .publish(&guard, &original.path, &target, &hash(raw.as_bytes()))?;
        let mut out = Outcome::note(&Record::from_text(target, raw.clone()), true);
        out.diagnostics = diagnostics;
        out.recovery = plan.recovery_paths();
        Ok(out)
    }
    pub fn new_note(&self, title: &str, body: &str, link: Option<&str>) -> Result<Outcome> {
        let draft = self.capture(title, body)?;
        let id = draft.data["id"].as_str().unwrap();
        self.publish(id, link).map_err(|mut error| {
            error.saved = true;
            if error.data.is_none() {
                error.data = Some(Box::new(draft.data));
            }
            error
        })
    }

    pub fn edit(&self, id: &str, argv: &[String]) -> Result<Outcome> {
        let (original, plan) = {
            let guard = self.vault.lock()?;
            self.supported()?;
            let catalog = self.scan()?;
            let original = self.mutable(&catalog, id)?.clone();
            let plan = self.vault.begin_edit(&guard, &original.path)?;
            (original, plan)
        };
        let recovery = plan.recovery_paths();
        let operation = (|| {
            let result = editor::run(argv, &self.vault.path(&plan.work)?, self.vault.root());
            File::open(self.vault.path(&plan.work)?)?.sync_all()?;
            result?;
            let bytes = self.vault.read(&plan.work)?;
            let raw = String::from_utf8(bytes)
                .map_err(|_| Error::new("validation_failed", "Рабочая копия не UTF-8"))?;
            let mut note = format::parse(&raw);
            validate_note_id(&note, id)?;
            if note.body != original.note.body {
                update_timestamp(&mut note);
            }
            let normalized = format::render(&note.fields, &note.body)
                .map_err(|e| Error::new("validation_failed", e))?;
            let changed = original.raw.as_deref() != Some(&normalized);
            let guard = self.vault.lock()?;
            self.supported()?;
            if hash(&self.vault.read(&original.path)?) != plan.base_hash {
                return Err(Error::new(
                    "conflict",
                    "Оригинал изменился во время редактора; рабочая копия сохранена",
                ));
            }
            let catalog = self.scan()?;
            self.mutable(&catalog, id)?;
            self.vault
                .finish_edit(&guard, &plan.operation_id, normalized.as_bytes())?;
            Ok(Outcome::note(
                &Record::from_text(original.path.clone(), normalized),
                changed,
            ))
        })();
        operation
            .map(|mut out| {
                out.recovery = recovery.clone();
                out
            })
            .map_err(|e: Error| e.with_recovery(recovery))
    }

    pub fn doctor_at(
        path: &Path,
        operation: Option<&str>,
        mode: Option<RecoveryMode>,
    ) -> Result<Outcome> {
        let vault = Vault::open(path)?;
        let Some(id) = operation else {
            if mode.is_some() {
                return Err(Error::new("usage", "Укажите ID операции"));
            }
            let plans = vault.plans()?;
            return Ok(Outcome::ok(
                json!({"operations":plans.into_iter().filter(Plan::unfinished).collect::<Vec<_>>()}),
            ));
        };
        let mode = mode.ok_or_else(|| Error::new("usage", "Выберите resume или restore"))?;
        let guard = vault.lock()?;
        let plan = vault.plan(id)?;
        let recovery = plan.recovery_paths();
        let operation = (|| {
            if plan.state == PlanState::Complete && matches!(mode, RecoveryMode::Resume) {
                return Ok(Outcome::ok(json!({"operation_id":id,"changed":false})));
            }
            let config =
                if plan.kind == PlanKind::Init {
                    VaultConfig::parse(plan.config.as_deref().ok_or_else(|| {
                        Error::new("recovery_required", "В init-плане нет config")
                    })?)?
                } else {
                    VaultConfig::load(&vault)?
                };
            config.supported()?;
            let restoring =
                matches!(mode, RecoveryMode::Restore) || plan.state == PlanState::Restoring;
            if restoring
                && plan.state == PlanState::Preparing
                && plan.kind != PlanKind::Init
                && !vault.path(&plan.work)?.try_exists()?
            {
                let recovered = vault.recover(&guard, id, RecoveryMode::Restore)?;
                return Ok(Outcome::ok(
                    json!({"operation_id":id,"state":recovered.state,"changed":false}),
                ));
            }
            let payload = if restoring { &plan.backup } else { &plan.work };
            if plan.kind != PlanKind::Init || matches!(mode, RecoveryMode::Resume) {
                let bytes = match vault.read(payload) {
                    Ok(bytes) => bytes,
                    Err(_)
                        if plan.state == PlanState::Preparing
                            && !vault.path(payload)?.try_exists()? =>
                    {
                        if plan.kind == PlanKind::Init {
                            plan.initial_note
                                .as_deref()
                                .ok_or_else(|| {
                                    Error::new("recovery_required", "Нет исходной root-заметки")
                                })?
                                .as_bytes()
                                .to_vec()
                        } else if plan.base_hash == plan.new_hash {
                            vault.read(&plan.from)?
                        } else {
                            return Err(Error::new(
                                "recovery_required",
                                "Подготовка новой версии прервана; выполните restore",
                            ));
                        }
                    }
                    Err(error) => return Err(error),
                };
                let raw = String::from_utf8(bytes)
                    .map_err(|_| Error::new("validation_failed", "Recovery payload не UTF-8"))?;
                let expected = if plan.kind == PlanKind::Init {
                    config.root_id.as_str()
                } else {
                    filename_id(&plan.from).ok_or_else(|| {
                        Error::new("validation_failed", "Recovery source не содержит ID")
                    })?
                };
                let note = format::parse(&raw);
                validate_note_id(&note, expected)?;
                if !restoring && matches!(plan.kind, PlanKind::Edit | PlanKind::Replace) {
                    let app = Self {
                        vault: vault.clone(),
                        config: config.clone(),
                    };
                    if app
                        .scan_with_config(&config, false)?
                        .records
                        .iter()
                        .any(|r| r.path != plan.from && r.path != plan.to && r.claims_id(expected))
                    {
                        return Err(Error::new(
                            "duplicate_id",
                            "Recovery ID занят другим файлом",
                        ));
                    }
                }
                if plan.kind == PlanKind::Publish || plan.kind == PlanKind::Init {
                    let app = Self {
                        vault: vault.clone(),
                        config: config.clone(),
                    };
                    let mut catalog = app.scan_with_config(&config, true)?;
                    catalog
                        .records
                        .retain(|r| r.path != plan.from && r.path != plan.to);
                    let record = Record::from_text(plan.to.clone(), raw);
                    if record.id() != Some(expected) {
                        return Err(Error::new(
                            "validation_failed",
                            "Recovery target не соответствует ID",
                        ));
                    }
                    if catalog.records.iter().any(|r| r.claims_id(expected)) {
                        return Err(Error::new(
                            "duplicate_id",
                            "Recovery ID занят другим файлом",
                        ));
                    }
                    catalog.records.push(record.clone());
                    if !restoring {
                        let diagnostics = publish_diagnostics(
                            &vault,
                            &catalog.records,
                            &record,
                            &plan.to,
                            &config.root_id,
                        );
                        if diagnostics.iter().any(|d| d.severity == Severity::Error) {
                            let mut e = Error::new(
                                "publish_rejected",
                                "Recovery не проходит проверки публикации",
                            );
                            e.diagnostics = diagnostics;
                            return Err(e);
                        }
                    }
                }
            }
            let recovered = vault.recover(&guard, id, mode)?;
            let mut out = Outcome::ok(
                json!({"operation_id":id,"state":recovered.state,"changed":true,"path":recovered.to}),
            );
            out.saved = true;
            Ok(out)
        })();
        operation
            .map(|mut out| {
                out.recovery = recovery.clone();
                out
            })
            .map_err(|e: Error| e.with_recovery(recovery))
    }
}

fn validate_note_id(note: &Note, id: &str) -> Result<()> {
    if !note.writable() || note.id() != Some(id) {
        let mut error = Error::new(
            "validation_failed",
            "Рабочая копия имеет структурные ошибки или изменённый ID",
        );
        error.diagnostics = note.diagnostics.clone();
        if note.id() != Some(id) {
            error.diagnostics.push(Diagnostic::new(
                "zetto/immutable-id",
                Severity::Error,
                "ID существующей заметки менять нельзя",
            ));
        }
        return Err(error);
    }
    Ok(())
}
fn publish_diagnostics(
    vault: &Vault,
    records: &[Record],
    record: &Record,
    source: &str,
    root: &str,
) -> Vec<Diagnostic> {
    let mut diagnostics = record.note.diagnostics.clone();
    let mut connected = false;
    if record.note.title().is_none_or(|t| t.trim().is_empty()) {
        push_diagnostic(
            &mut diagnostics,
            Diagnostic::new(
                "zetto/empty-title",
                Severity::Error,
                "Для публикации нужен заголовок",
            ),
        );
    }
    for link in &record.note.links {
        let resolution = resolver::resolve(vault, records, source, link);
        if let Some(target) = resolution.target {
            connected |= records[target].stage == Stage::Published
                && records[target].note.id() != record.note.id();
        }
        if let Some(mut d) = resolution.diagnostic {
            if !matches!(
                resolution.kind,
                ResolutionKind::Resolved | ResolutionKind::External | ResolutionKind::Attachment
            ) {
                d.severity = Severity::Error;
            }
            push_diagnostic(&mut diagnostics, d);
        }
    }
    if record.note.id() == Some(root) {
        push_diagnostic(
            &mut diagnostics,
            Diagnostic::new(
                "zetto/root-exception",
                Severity::Info,
                "Явное исключение для корня",
            ),
        );
    } else if !connected {
        push_diagnostic(
            &mut diagnostics,
            Diagnostic::new(
                "zetto/link-before-publish",
                Severity::Error,
                "Нужна связь на другую опубликованную заметку",
            ),
        );
    }
    for d in &mut diagnostics {
        d.path = Some(record.path.clone());
    }
    diagnostics
}
fn push_diagnostic(diagnostics: &mut Vec<Diagnostic>, diagnostic: Diagnostic) {
    if let Some(existing) = diagnostics
        .iter_mut()
        .find(|d| d.rule_id == diagnostic.rule_id && d.span == diagnostic.span)
    {
        if diagnostic.severity == Severity::Error {
            *existing = diagnostic;
        }
    } else {
        diagnostics.push(diagnostic);
    }
}
fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
fn update_timestamp(note: &mut Note) {
    if note.fields.get("x-skip-updated") == Some(&Value::Bool(true)) {
        return;
    }
    if note.fields.get("updated").is_some_and(|v| {
        v.as_str()
            .is_none_or(|s| chrono::DateTime::parse_from_rfc3339(s).is_err())
    }) {
        return;
    }
    note.fields.insert("updated".into(), json!(now()));
}
