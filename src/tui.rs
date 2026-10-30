mod worker;
use crate::app::{App, Catalog, Outcome, Record, Stage};
use crate::error::{Error, Result};
use crate::index::FreshnessMode;
use crate::interrupt;
use crate::safe;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, EnableBracketedPaste, EnableFocusChange,
    Event, KeyCode, KeyEventKind, KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use std::collections::BTreeMap;
use std::io;
use std::sync::mpsc::{RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;
use worker::{Data, Lane, Read, Reply, Worker, Write};

// Drop и hook используют одну идемпотентную последовательность, в том числе при panic.
fn restore() {
    let _ = terminal::disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        DisableBracketedPaste,
        DisableFocusChange,
        LeaveAlternateScreen,
        Show
    );
}
struct Session;
impl Session {
    fn resume(&self) -> Result<()> {
        terminal::enable_raw_mode()?;
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableBracketedPaste,
            EnableFocusChange,
            Hide
        )?;
        Ok(())
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        restore();
    }
}

#[derive(Default)]
struct Form {
    title: String,
    body: String,
    title_focus: bool,
    publish: bool,
    confirm: Option<usize>,
}
impl Form {
    fn pending(&self) -> bool {
        !self.title.is_empty() || !self.body.is_empty()
    }
    fn field(&mut self) -> &mut String {
        if self.title_focus {
            &mut self.title
        } else {
            &mut self.body
        }
    }
    fn insert(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        if self.title_focus {
            self.title.extend(text.chars().filter(|c| !c.is_control()));
        } else {
            self.body.extend(
                text.chars()
                    .filter(|c| !c.is_control() || matches!(c, '\n' | '\t')),
            );
        }
    }
}
struct Picker {
    source: String,
    candidates: Vec<(String, String)>,
    selected: usize,
    publish: bool,
    query: String,
    editing: bool,
    loading: bool,
}
#[derive(Default)]
struct Search {
    query: String,
    editing: bool,
    paths: Vec<String>,
    loading: bool,
    truncated: bool,
}
enum Change {
    Capture {
        publish: bool,
    },
    Mutation {
        source: String,
        publish: bool,
        editor: bool,
    },
}
struct Pending {
    id: u64,
    change: Change,
}
#[derive(Clone, Copy)]
enum Exit {
    Normal,
    Interrupt,
}
struct Ui {
    catalog: Catalog,
    catalog_loading: bool,
    by_path: BTreeMap<String, usize>,
    select_id: Option<String>,
    inbox: bool,
    selected: usize,
    preview: bool,
    body: Option<Record>,
    body_path: Option<String>,
    body_loading: bool,
    body_error: Option<String>,
    scroll: u16,
    form: Option<Form>,
    picker: Option<Picker>,
    search: Option<Search>,
    pending: Option<Pending>,
    exit: Option<Exit>,
    last_saved: Option<Outcome>,
    help: bool,
    watch_warning: Option<String>,
    message: String,
}
impl Ui {
    fn new() -> Self {
        Self {
            catalog: Catalog {
                freshness: Default::default(),
                metrics: Default::default(),
                records: Vec::new(),
                warnings: Vec::new(),
                complete: false,
            },
            catalog_loading: false,
            by_path: BTreeMap::new(),
            select_id: None,
            inbox: false,
            selected: 0,
            preview: false,
            body: None,
            body_path: None,
            body_loading: false,
            body_error: None,
            scroll: 0,
            form: None,
            picker: None,
            search: None,
            pending: None,
            exit: None,
            last_saved: None,
            help: false,
            watch_warning: None,
            message: "c — записать мысль · / поиск · r — сверить файлы".into(),
        }
    }
    fn records(&self) -> Vec<&Record> {
        if let Some(search) = &self.search
            && !search.query.is_empty()
        {
            return search
                .paths
                .iter()
                .filter_map(|path| self.by_path.get(path).map(|i| &self.catalog.records[*i]))
                .filter(|r| (r.stage == Stage::Inbox) == self.inbox)
                .collect();
        }
        self.catalog
            .records
            .iter()
            .filter(|r| (r.stage == Stage::Inbox) == self.inbox)
            .collect()
    }
    fn current(&self) -> Option<&Record> {
        self.records().get(self.selected).copied()
    }
    fn refresh(&mut self, worker: &mut Worker, mode: FreshnessMode, selected: Option<String>) {
        if selected.is_some() {
            self.select_id = selected;
        }
        self.catalog_loading = true;
        worker.read(Read::Catalog(mode));
    }
    fn preview(&mut self, worker: &mut Worker, force: bool) {
        let path = self.current().map(|r| r.path.clone());
        if !force && path == self.body_path {
            return;
        }
        self.body_path = path.clone();
        self.body = None;
        self.body_error = None;
        self.body_loading = path.is_some();
        if let Some(path) = path {
            worker.read(Read::Preview(path));
        } else {
            worker.cancel(Lane::Preview);
        }
    }
    fn query(&mut self, worker: &mut Worker) {
        if let Some(search) = &mut self.search {
            search.loading = !search.query.is_empty();
            if search.loading {
                worker.read(Read::Search(search.query.clone()));
            } else {
                search.paths.clear();
                worker.cancel(Lane::Search);
            }
        }
        self.selected = 0;
        self.preview(worker, true);
    }
    fn fuzzy(&mut self, worker: &mut Worker) {
        if let Some(picker) = &mut self.picker {
            picker.loading = true;
            worker.read(Read::Fuzzy(picker.query.clone()));
        }
    }
    fn pick(&mut self, worker: &mut Worker, source: String, publish: bool) {
        self.picker = Some(Picker {
            source,
            candidates: Vec::new(),
            selected: 0,
            publish,
            query: String::new(),
            editing: false,
            loading: true,
        });
        self.fuzzy(worker);
    }
    fn report(&mut self, result: Result<Outcome>) {
        self.message = match result {
            Ok(out) => outcome_text(&out),
            Err(error) => error_text(&error),
        };
    }
    fn writing(&mut self) {
        self.catalog_loading = false;
        self.body_loading = false;
        if let Some(search) = &mut self.search {
            search.loading = false;
        }
        if let Some(picker) = &mut self.picker {
            picker.loading = false;
        }
    }
    fn save(&mut self, worker: &mut Worker, publish: bool) -> Result<()> {
        if self.pending.is_some() {
            return Ok(());
        }
        let Some(form) = &self.form else {
            return Ok(());
        };
        if !form.pending() {
            self.form = None;
            return Ok(());
        }
        let write = Write::Capture {
            title: form.title.clone(),
            body: form.body.clone(),
        };
        self.writing();
        let id = worker.write(write)?;
        self.last_saved = None;
        self.pending = Some(Pending {
            id,
            change: Change::Capture { publish },
        });
        self.message = "Сохранение… ввод ожидает подтверждения".into();
        Ok(())
    }
    fn mutate(
        &mut self,
        worker: &mut Worker,
        write: Write,
        source: String,
        publish: bool,
        editor: bool,
    ) -> Result<()> {
        self.writing();
        let id = worker.write(write)?;
        self.pending = Some(Pending {
            id,
            change: Change::Mutation {
                source,
                publish,
                editor,
            },
        });
        self.message = if editor {
            "Редактор…"
        } else {
            "Сохранение…"
        }
        .into();
        Ok(())
    }
    fn exiting(&mut self, worker: &mut Worker, exit: Exit) {
        self.exit = Some(exit);
        worker.cancel_reads();
        self.catalog_loading = false;
        if let Err(error) = self.save(worker, false) {
            self.failed_exit(error);
        }
    }
    fn failed_exit(&mut self, error: Error) {
        self.exit = None;
        interrupt::clear();
        self.message = format!(
            "{}\nТекст НЕ подтверждён, форма остаётся открытой",
            error_text(&error)
        );
    }
    fn finished(&self) -> Option<Result<()>> {
        if self.pending.is_some() {
            return None;
        }
        match self.exit? {
            Exit::Normal => Some(Ok(())),
            Exit::Interrupt => {
                let mut error = Error::new(
                    "interrupted",
                    if self.last_saved.is_some() {
                        "Прервано; принятый ввод сохранён"
                    } else {
                        "Прервано пользователем"
                    },
                );
                if let Some(out) = &self.last_saved {
                    saved_error(&mut error, out.clone());
                }
                Some(Err(error))
            }
        }
    }
    // Возвращает true после ответа редактора: только тогда UI снова владеет TTY.
    fn reply(&mut self, worker: &mut Worker, reply: Reply, mode: FreshnessMode) -> bool {
        if let Some(lane) = reply.lane {
            if !worker.current(lane, reply.id) {
                return false;
            }
            match lane {
                Lane::Catalog => {
                    self.catalog_loading = false;
                    match reply.result {
                        Ok(Data::Catalog(catalog)) => {
                            let path = self.current().map(|r| r.path.clone());
                            let old_id = self.current().and_then(Record::id).map(str::to_owned);
                            let changed = self.catalog.freshness.generation.is_none()
                                || self.catalog.freshness.generation
                                    != catalog.freshness.generation;
                            self.catalog = catalog;
                            self.by_path = self
                                .catalog
                                .records
                                .iter()
                                .enumerate()
                                .map(|(i, r)| (r.path.clone(), i))
                                .collect();
                            let records = self.records();
                            self.selected = records
                                .iter()
                                .position(|r| {
                                    self.select_id.as_ref().is_some_and(|id| r.id() == Some(id))
                                        || (self.select_id.is_none()
                                            && (path.as_deref() == Some(&r.path)
                                                || old_id
                                                    .as_ref()
                                                    .is_some_and(|id| r.id() == Some(id))))
                                })
                                .unwrap_or(self.selected.min(records.len().saturating_sub(1)));
                            self.select_id = None;
                            self.preview(worker, changed || self.body_error.is_some());
                            if changed
                                && let Some(search) = &mut self.search
                                && !search.query.is_empty()
                            {
                                search.loading = true;
                                worker.read(Read::Search(search.query.clone()));
                            }
                            if self.message == "Сверка…" {
                                self.message = "Файлы сверены".into();
                            }
                            if changed && self.picker.is_some() {
                                self.fuzzy(worker);
                            }
                        }
                        Err(error) => {
                            self.message = if let Some(saved) = &self.last_saved {
                                format!("{}\nСверка: {}", outcome_text(saved), error_text(&error))
                            } else {
                                error_text(&error)
                            }
                        }
                        _ => {}
                    }
                }
                Lane::Preview => {
                    self.body_loading = false;
                    match reply.result {
                        Ok(Data::Preview(record))
                            if Some(&record.path) == self.body_path.as_ref() =>
                        {
                            self.body = Some(record)
                        }
                        Err(error) => self.body_error = Some(error_text(&error)),
                        _ => {}
                    }
                }
                Lane::Search => {
                    let path = self.current().map(|r| r.path.clone());
                    if let Some(search) = &mut self.search {
                        search.loading = false;
                        match reply.result {
                            Ok(Data::Outcome(out)) => {
                                search.truncated = out.truncated;
                                search.paths = items(&out)
                                    .filter_map(|v| v["path"].as_str().map(str::to_owned))
                                    .collect();
                            }
                            Err(error) => {
                                search.paths.clear();
                                self.message = error_text(&error);
                            }
                            _ => {}
                        }
                        self.selected = self
                            .records()
                            .iter()
                            .position(|r| path.as_deref() == Some(&r.path))
                            .unwrap_or(self.selected.min(self.records().len().saturating_sub(1)));
                        self.preview(worker, true);
                    }
                }
                Lane::Fuzzy => {
                    if let Some(picker) = &mut self.picker {
                        picker.loading = false;
                        match reply.result {
                            Ok(Data::Outcome(out)) => {
                                picker.candidates = items(&out)
                                    .filter_map(|v| {
                                        Some((
                                            v["id"].as_str()?.to_owned(),
                                            v["title"]
                                                .as_str()
                                                .unwrap_or("Без заголовка")
                                                .to_owned(),
                                        ))
                                    })
                                    .filter(|(id, _)| id != &picker.source)
                                    .collect();
                                picker.selected = 0;
                            }
                            Err(error) => {
                                picker.candidates.clear();
                                self.message = error_text(&error);
                            }
                            _ => {}
                        }
                    }
                }
            }
            return false;
        }
        if self.pending.as_ref().is_none_or(|p| p.id != reply.id) {
            return false;
        }
        let pending = self.pending.take().unwrap();
        let editor = matches!(pending.change, Change::Mutation { editor: true, .. });
        match reply.result {
            Ok(Data::Outcome(out)) => {
                self.last_saved = Some(out.clone());
                let source = match pending.change {
                    Change::Capture { publish } => {
                        let id = out.data["id"].as_str().unwrap_or("").to_owned();
                        self.form = None;
                        self.inbox = true;
                        self.search = None;
                        worker.cancel(Lane::Search);
                        if publish && self.exit.is_none() {
                            self.pick(worker, id.clone(), true);
                        }
                        id
                    }
                    Change::Mutation {
                        source, publish, ..
                    } => {
                        if publish {
                            self.inbox = false;
                        }
                        source
                    }
                };
                self.report(Ok(out));
                if self.exit.is_none() {
                    self.refresh(worker, mode, Some(source));
                }
            }
            Err(error) => {
                if error.saved && matches!(pending.change, Change::Capture { .. }) {
                    self.form = None;
                }
                if self.exit.is_some() {
                    self.failed_exit(error);
                } else {
                    self.report(Err(error));
                    self.refresh(worker, mode, None);
                }
            }
            _ => {}
        }
        editor
    }
}
fn same_catalog(a: &Catalog, b: &Catalog) -> bool {
    a.freshness.generation.is_some()
        && a.freshness.generation == b.freshness.generation
        && (a.freshness.mode == FreshnessMode::Cached)
            == (b.freshness.mode == FreshnessMode::Cached)
        && a.complete == b.complete
        && a.warnings.len() == b.warnings.len()
        && a.warnings
            .iter()
            .zip(&b.warnings)
            .all(|(a, b)| a.code == b.code && a.message == b.message && a.path == b.path)
}
fn items(out: &Outcome) -> impl Iterator<Item = &serde_json::Value> {
    out.data["items"].as_array().into_iter().flatten()
}
fn outcome_text(out: &Outcome) -> String {
    let mut message = format!(
        "Сохранено {} {}{}",
        out.data["id"].as_str().unwrap_or(""),
        out.data["path"].as_str().unwrap_or(""),
        diagnostics(&out.diagnostics)
    );
    for warning in &out.warnings {
        message.push_str(&format!("\n! {}: {}", warning.code, warning.message));
    }
    message
}
fn diagnostics(items: &[crate::note::Diagnostic]) -> String {
    items
        .iter()
        .map(|d| format!("\n{}: {}", d.rule_id, d.message))
        .collect()
}
fn error_text(e: &Error) -> String {
    format!(
        "! {}: {}{}{}",
        e.code,
        e.message,
        diagnostics(&e.diagnostics),
        if e.recovery.is_empty() {
            String::new()
        } else {
            format!("\nВосстановление: {}", e.recovery.join(", "))
        }
    )
}

fn backspace(text: &mut String) {
    if let Some((offset, _)) = text.grapheme_indices(true).next_back() {
        text.truncate(offset);
    }
}

pub fn run(app: App, color: bool) -> Result<()> {
    let initial_mode = app.freshness_mode();
    let mode = if initial_mode == FreshnessMode::Full {
        FreshnessMode::Stat
    } else {
        initial_mode
    };
    let mut ui = Ui::new();
    let mut worker = Worker::start(app.clone());
    ui.refresh(&mut worker, initial_mode, None);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));
    let session = Session;
    session.resume()?;
    let mut screen = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    screen.clear()?;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        let mut dirty = true;
        let mut editor_active = false;
        let mut drawn_catalog_loading = false;
        let mut reconcile = Instant::now();
        let mut watch_due = None;
        let mut watch_full = false;
        loop {
            if interrupt::requested() && ui.exit.is_none() {
                ui.exiting(&mut worker, Exit::Interrupt);
                dirty = true;
            }
            worker.flush();
            // Пока редактор владеет TTY, не читаем stdin и не рисуем даже при notify.
            if editor_active {
                match worker.rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(reply) => {
                        if ui.reply(&mut worker, reply, mode) {
                            editor_active = false;
                            session.resume()?;
                            screen.clear()?;
                        }
                        dirty = true;
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => {
                        return Err(Error::new("io", "Рабочий поток завершён"));
                    }
                }
                continue;
            }
            loop {
                match worker.rx.try_recv() {
                    Ok(reply) => {
                        // Устаревший ответ не вызывает даже декоративную перерисовку.
                        if reply.lane.is_none_or(|lane| worker.current(lane, reply.id)) {
                            let catalog_error =
                                matches!(reply.lane, Some(Lane::Catalog)) && reply.result.is_err();
                            let quiet = !drawn_catalog_loading
                                && ui.select_id.is_none()
                                && match &reply.result {
                                    Ok(Data::Catalog(catalog)) => {
                                        same_catalog(&ui.catalog, catalog)
                                    }
                                    _ => false,
                                };
                            let message = ui.message.clone();
                            ui.reply(&mut worker, reply, mode);
                            dirty |= !(quiet
                                || (catalog_error
                                    && !drawn_catalog_loading
                                    && message == ui.message));
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        return Err(Error::new("io", "Рабочий поток завершён"));
                    }
                }
            }
            if let Some(result) = ui.finished() {
                return result;
            }
            if let Some(warning) = worker.warning() {
                ui.watch_warning = Some(warning);
                dirty = true;
            }
            if let Some(freshness) = worker.watch.take() {
                watch_full |= freshness == FreshnessMode::Full;
                watch_due.get_or_insert_with(Instant::now);
            }
            let periodic = reconcile.elapsed() >= Duration::from_secs(5);
            let notified =
                watch_due.is_some_and(|due: Instant| due.elapsed() >= Duration::from_millis(150));
            if ui.exit.is_none() && !ui.catalog_loading && (periodic || notified) {
                let freshness = if mode == FreshnessMode::Cached {
                    mode
                } else if periodic || watch_full {
                    FreshnessMode::Full
                } else {
                    FreshnessMode::Stat
                };
                ui.refresh(&mut worker, freshness, None);
                reconcile = Instant::now();
                watch_due = None;
                watch_full = false;
                // Не перерисовываем одинаковый экран по таймеру: ответ изменит его по событию.
            }
            if dirty {
                screen.draw(|frame| draw(frame, &ui, color))?;
                drawn_catalog_loading = ui.catalog_loading;
                dirty = false;
            }
            if !event::poll(Duration::from_millis(50))? {
                continue;
            }
            let event = event::read()?;
            dirty = true;
            match event {
                Event::Resize(_, _) => screen.clear()?,
                Event::FocusGained => ui.refresh(
                    &mut worker,
                    if mode == FreshnessMode::Cached {
                        mode
                    } else {
                        FreshnessMode::Full
                    },
                    None,
                ),
                Event::Paste(text) if ui.pending.is_none() => {
                    if let Some(form) = ui.form.as_mut() {
                        form.insert(&text);
                    } else if let Some(picker) = ui.picker.as_mut().filter(|p| p.editing) {
                        picker
                            .query
                            .extend(text.chars().filter(|c| !c.is_control()));
                        ui.fuzzy(&mut worker);
                    } else if let Some(search) = ui.search.as_mut().filter(|s| s.editing) {
                        search
                            .query
                            .extend(text.chars().filter(|c| !c.is_control()));
                        ui.query(&mut worker);
                    }
                }
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        ui.exiting(&mut worker, Exit::Interrupt);
                        continue;
                    }
                    if ui.pending.is_some() {
                        if key.code == KeyCode::Char('q') {
                            ui.exiting(&mut worker, Exit::Normal);
                        }
                        continue;
                    }
                    let size = screen.size()?;
                    if size.width < 60 || size.height < 12 {
                        if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                            ui.exiting(&mut worker, Exit::Normal);
                        }
                        continue;
                    }
                    if ui.help {
                        if matches!(
                            key.code,
                            KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')
                        ) {
                            ui.help = false;
                        }
                        continue;
                    }
                    if let Some(form) = ui.form.as_mut() {
                        if let Some(choice) = form.confirm {
                            match key.code {
                                KeyCode::Left | KeyCode::Up => {
                                    form.confirm = Some(choice.saturating_sub(1))
                                }
                                KeyCode::Right | KeyCode::Down => {
                                    form.confirm = Some((choice + 1).min(2))
                                }
                                KeyCode::Esc | KeyCode::Char('c') => form.confirm = None,
                                KeyCode::Char('d') => ui.form = None,
                                KeyCode::Char('s') | KeyCode::Enter
                                    if key.code == KeyCode::Char('s') || choice == 0 =>
                                {
                                    if let Err(error) = ui.save(&mut worker, false) {
                                        ui.message = error_text(&error);
                                    }
                                }
                                KeyCode::Enter if choice == 2 => ui.form = None,
                                KeyCode::Enter => form.confirm = None,
                                _ => {}
                            }
                        } else if key.code == KeyCode::F(2)
                            || (key.code == KeyCode::Char('s')
                                && key.modifiers.contains(KeyModifiers::CONTROL))
                            || (key.code == KeyCode::Enter
                                && key.modifiers.contains(KeyModifiers::CONTROL))
                        {
                            let publish = form.publish;
                            if let Err(error) = ui.save(&mut worker, publish) {
                                ui.message = error_text(&error);
                            }
                        } else {
                            match key.code {
                                KeyCode::Esc if form.pending() => form.confirm = Some(1),
                                KeyCode::Esc => ui.form = None,
                                KeyCode::Tab | KeyCode::BackTab => {
                                    form.title_focus = !form.title_focus
                                }
                                KeyCode::Backspace => backspace(form.field()),
                                KeyCode::Enter if form.title_focus => form.title_focus = false,
                                KeyCode::Enter => form.body.push('\n'),
                                KeyCode::Char(c)
                                    if !key
                                        .modifiers
                                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                                {
                                    form.insert(&c.to_string())
                                }
                                _ => {}
                            }
                        }
                        continue;
                    }
                    if let Some(picker) = ui.picker.as_mut() {
                        let mut query_changed = false;
                        if picker.editing {
                            match key.code {
                                KeyCode::Esc => picker.editing = false,
                                KeyCode::Backspace => {
                                    backspace(&mut picker.query);
                                    query_changed = true;
                                }
                                KeyCode::Char(c)
                                    if !key
                                        .modifiers
                                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                                {
                                    picker.query.push(c);
                                    query_changed = true;
                                }
                                KeyCode::Enter | KeyCode::Up | KeyCode::Down => {}
                                _ => continue,
                            }
                            if query_changed {
                                ui.fuzzy(&mut worker);
                                continue;
                            }
                            if key.code == KeyCode::Esc {
                                continue;
                            }
                        }
                        match key.code {
                            KeyCode::Char('/') => picker.editing = true,
                            KeyCode::Up | KeyCode::Char('k') => {
                                picker.selected = picker.selected.saturating_sub(1)
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                picker.selected = (picker.selected + 1)
                                    .min(picker.candidates.len().saturating_sub(1))
                            }
                            KeyCode::Enter if picker.loading => {
                                ui.message = "Поиск связи… дождитесь актуального результата".into()
                            }
                            KeyCode::Enter | KeyCode::Esc => {
                                let picker = ui.picker.take().unwrap();
                                worker.cancel(Lane::Fuzzy);
                                let target = if key.code == KeyCode::Enter {
                                    picker
                                        .candidates
                                        .get(picker.selected)
                                        .map(|(id, _)| id.clone())
                                } else {
                                    None
                                };
                                let write = if picker.publish {
                                    Some(Write::Publish {
                                        source: picker.source.clone(),
                                        target,
                                    })
                                } else {
                                    target.map(|target| Write::Link {
                                        source: picker.source.clone(),
                                        target,
                                    })
                                };
                                if let Some(write) = write
                                    && let Err(error) = ui.mutate(
                                        &mut worker,
                                        write,
                                        picker.source,
                                        picker.publish,
                                        false,
                                    )
                                {
                                    ui.message = error_text(&error);
                                }
                            }
                            _ => {}
                        }
                        continue;
                    }
                    if let Some(search) = ui.search.as_mut().filter(|s| s.editing) {
                        match key.code {
                            KeyCode::Esc => {
                                ui.search = None;
                                worker.cancel(Lane::Search);
                                ui.selected = 0;
                                ui.preview(&mut worker, true);
                            }
                            KeyCode::Enter => {
                                if !search.loading {
                                    search.editing = false;
                                }
                            }
                            KeyCode::Backspace => {
                                backspace(&mut search.query);
                                ui.query(&mut worker);
                            }
                            KeyCode::Char(c)
                                if !key
                                    .modifiers
                                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                            {
                                search.query.push(c);
                                ui.query(&mut worker);
                            }
                            _ => {}
                        }
                        continue;
                    }
                    match key.code {
                        KeyCode::Char('q') => ui.exiting(&mut worker, Exit::Normal),
                        KeyCode::Char('?') => ui.help = true,
                        KeyCode::Char('/') => {
                            ui.search.get_or_insert_with(Search::default).editing = true;
                        }
                        KeyCode::Esc if ui.search.is_some() => {
                            ui.search = None;
                            worker.cancel(Lane::Search);
                            ui.selected = 0;
                            ui.preview(&mut worker, true);
                        }
                        KeyCode::Char('i') | KeyCode::Esc => {
                            ui.inbox = !ui.inbox;
                            ui.selected = 0;
                            ui.scroll = 0;
                            ui.preview(&mut worker, true);
                        }
                        KeyCode::Tab | KeyCode::BackTab => ui.preview = !ui.preview,
                        KeyCode::Char('c') => {
                            ui.last_saved = None;
                            ui.form = Some(Form::default());
                        }
                        KeyCode::Char('n') => {
                            ui.last_saved = None;
                            ui.form = Some(Form {
                                title_focus: true,
                                publish: true,
                                ..Form::default()
                            })
                        }
                        KeyCode::Char('r') => {
                            ui.refresh(&mut worker, FreshnessMode::Full, None);
                            ui.message = "Сверка…".into();
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            if ui.preview {
                                ui.scroll = ui.scroll.saturating_add(1);
                            } else {
                                ui.selected =
                                    (ui.selected + 1).min(ui.records().len().saturating_sub(1));
                                ui.scroll = 0;
                                ui.preview(&mut worker, false);
                            }
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            if ui.preview {
                                ui.scroll = ui.scroll.saturating_sub(1);
                            } else {
                                ui.selected = ui.selected.saturating_sub(1);
                                ui.scroll = 0;
                                ui.preview(&mut worker, false);
                            }
                        }
                        KeyCode::Char('l') | KeyCode::Char('p') | KeyCode::Char('e') => {
                            if let Some(id) = ui
                                .select_id
                                .clone()
                                .or_else(|| ui.current().and_then(Record::id).map(str::to_owned))
                            {
                                match key.code {
                                    KeyCode::Char('l') => ui.pick(&mut worker, id, false),
                                    KeyCode::Char('p') => {
                                        if let Err(error) = ui.mutate(
                                            &mut worker,
                                            Write::Publish {
                                                source: id.clone(),
                                                target: None,
                                            },
                                            id,
                                            true,
                                            false,
                                        ) {
                                            ui.message = error_text(&error);
                                        }
                                    }
                                    _ => {
                                        worker.cancel_reads();
                                        restore();
                                        match ui.mutate(
                                            &mut worker,
                                            Write::Edit { source: id.clone() },
                                            id,
                                            false,
                                            true,
                                        ) {
                                            Ok(()) => editor_active = true,
                                            Err(error) => {
                                                session.resume()?;
                                                screen.clear()?;
                                                ui.message = error_text(&error);
                                            }
                                        }
                                    }
                                }
                            } else {
                                ui.message =
                                    "! Нет однозначной структурно корректной заметки для действия"
                                        .into();
                            }
                        }
                        _ => {}
                    }
                }
                _ => dirty = false,
            }
        }
    }));
    match result {
        Ok(Err(mut error)) => {
            preserve(&mut ui, &mut worker, &app, &mut error);
            Err(error)
        }
        Ok(result) => result,
        Err(panic) => {
            let mut error = Error::new("io", "Аварийное завершение интерфейса");
            preserve(&mut ui, &mut worker, &app, &mut error);
            eprintln!(
                "{} {}",
                error,
                error
                    .data
                    .as_deref()
                    .map_or(String::new(), ToString::to_string)
            );
            std::panic::resume_unwind(panic)
        }
    }
}
fn preserve(ui: &mut Ui, worker: &mut Worker, app: &App, error: &mut Error) {
    restore();
    worker.cancel_reads();
    let accepted_capture = ui
        .pending
        .as_ref()
        .is_some_and(|p| matches!(p.change, Change::Capture { .. }));
    ui.exit = Some(Exit::Normal);
    // Сначала забираем результат принятой записи: второй capture создал бы дубликат.
    while ui.pending.is_some() {
        match worker.rx.recv() {
            Ok(reply) => {
                ui.reply(worker, reply, FreshnessMode::Cached);
            }
            Err(_) => {
                error
                    .message
                    .push_str("; нет подтверждения рабочего потока");
                break;
            }
        }
    }
    if let Some(out) = &ui.last_saved {
        saved_error(error, out.clone());
    }
    if !accepted_capture && let Some(form) = ui.form.as_ref().filter(|f| f.pending()) {
        match app.capture(&form.title, &form.body) {
            Ok(out) => {
                saved_error(error, out);
                ui.form = None;
            }
            Err(save_error) => error.message.push_str(&format!(
                "; текст НЕ подтверждён: {}",
                error_text(&save_error)
            )),
        }
    }
    if let Some(form) = &ui.form {
        error.data = Some(Box::new(
            serde_json::json!({"unsaved_title": form.title, "unsaved_body": form.body}),
        ));
    }
}
fn saved_error(error: &mut Error, out: Outcome) {
    error.saved = out.saved;
    error.context.warnings = out.warnings;
    error.context.freshness = out.freshness;
    error.data = Some(Box::new(out.data));
    error.recovery = out.recovery;
    error.diagnostics = out.diagnostics;
}
fn panel(title: &str) -> Block<'_> {
    Block::default().borders(Borders::ALL).title(title)
}
fn draw(frame: &mut Frame, ui: &Ui, color: bool) {
    let area = frame.area();
    let style = if color {
        Style::default()
            .fg(Color::Rgb(235, 219, 178))
            .bg(Color::Rgb(40, 40, 40))
    } else {
        Style::default()
    };
    frame.render_widget(Block::default().style(style), area);
    if area.width < 60 || area.height < 12 {
        frame.render_widget(
            Paragraph::new(format!(
                "Нужно ≥60×12. Ввод сохранён в памяти. q/Esc — сохранить и выйти.\n{}",
                safe(&ui.message)
            ))
            .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(5),
            Constraint::Length(1),
        ])
        .split(area);
    frame.render_widget(
        Paragraph::new(format!(
            "zetto / {} · {} · {} · {}{}",
            if ui.catalog_loading && ui.catalog.freshness.mode == FreshnessMode::NotApplicable {
                "Загрузка…"
            } else if ui.inbox {
                "Inbox"
            } else {
                "Library"
            },
            if ui.preview {
                "[ТЕКСТ]"
            } else {
                "[СПИСОК]"
            },
            ui.records().len(),
            if ui.catalog.freshness.mode == FreshnessMode::Cached {
                "cached"
            } else {
                "сверено"
            },
            if ui.catalog_loading {
                " · Сверка…"
            } else {
                ""
            }
        )),
        rows[0],
    );
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
        .split(rows[1]);
    let list_area = if area.width >= 80 {
        Some(columns[0])
    } else if !ui.preview {
        Some(rows[1])
    } else {
        None
    };
    let preview_area = if area.width >= 80 {
        Some(columns[1])
    } else if ui.preview {
        Some(rows[1])
    } else {
        None
    };
    if ui.form.is_none() && ui.picker.is_none() {
        if let Some(rect) = list_area {
            let items: Vec<_> = ui
                .records()
                .iter()
                .map(|r| {
                    ListItem::new(safe(
                        r.note
                            .title()
                            .filter(|t| !t.is_empty())
                            .unwrap_or("Без заголовка"),
                    ))
                })
                .collect();
            let mut state = ListState::default().with_selected(Some(ui.selected));
            let highlight = if color {
                Style::default().fg(Color::Rgb(216, 166, 87))
            } else {
                Style::default()
            };
            frame.render_stateful_widget(
                List::new(items)
                    .block(panel("Заметки"))
                    .highlight_symbol("> ")
                    .highlight_style(highlight),
                rect,
                &mut state,
            );
        }
        if let Some(rect) = preview_area {
            let text = ui
                .current()
                .map(|r| {
                    format!(
                        "{}\n{}\n{}{}\n{}",
                        r.note.title().unwrap_or("Без заголовка"),
                        r.note.id().unwrap_or("ID неверен"),
                        r.path,
                        if r.note
                            .diagnostics
                            .iter()
                            .any(|d| d.rule_id == "zetto/root-exception")
                        {
                            "\nИсключение: корень"
                        } else {
                            ""
                        },
                        if ui.body_loading {
                            "Загрузка предпросмотра…"
                        } else if let Some(error) = &ui.body_error {
                            error
                        } else {
                            ui.body.as_ref().map_or("", |body| body.note.body.as_str())
                        }
                    )
                })
                .unwrap_or_else(|| {
                    if ui.search.as_ref().is_some_and(|s| !s.query.is_empty()) {
                        "Нет совпадений".into()
                    } else {
                        "Нет заметок · c — записать мысль".into()
                    }
                });
            frame.render_widget(
                Paragraph::new(safe(&text))
                    .block(panel("Предпросмотр"))
                    .wrap(Wrap { trim: false })
                    .scroll((ui.scroll, 0)),
                rect,
            );
        }
    }
    let mut message = ui.message.clone();
    if let Some(warning) = &ui.watch_warning {
        message.push_str(&format!("\n{warning}"));
    }
    if let Some(search) = &ui.search {
        message = format!(
            "/ {} {}{}\n{}",
            safe(&search.query),
            if search.editing { "[ВВОД] " } else { "" },
            if search.loading {
                "Поиск…"
            } else if search.truncated {
                "Первые 1000 совпадений"
            } else {
                ""
            },
            message
        );
    }
    if let Some(record) = ui.current() {
        message.push_str(&diagnostics(&record.note.diagnostics));
    }
    for warning in &ui.catalog.warnings {
        message.push_str(&format!("\n! {}: {}", warning.code, warning.message));
    }
    frame.render_widget(
        Paragraph::new(safe(&message)).wrap(Wrap { trim: false }),
        rows[2],
    );
    frame.render_widget(
        Paragraph::new(
            "i Inbox/Library · / поиск · Tab фокус · c ввод · n новая · e l p · r сверка · ? · q",
        ),
        rows[3],
    );
    if let Some(form) = &ui.form {
        frame.render_widget(Clear, rows[1]);
        let input = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(1)])
            .split(rows[1]);
        frame.render_widget(
            Paragraph::new(safe(&form.title)).block(panel(if form.title_focus {
                "> Заголовок · Tab → текст"
            } else {
                "Заголовок"
            })),
            input[0],
        );
        let text = safe(&form.body);
        let visible = text
            .lines()
            .count()
            .saturating_sub(input[1].height.saturating_sub(2) as usize)
            .min(u16::MAX as usize) as u16;
        frame.render_widget(
            Paragraph::new(text)
                .block(panel(if form.title_focus {
                    "Текст"
                } else {
                    "> Текст · Enter строка · Ctrl-S/F2 сохранить · Esc отмена"
                }))
                .wrap(Wrap { trim: false })
                .scroll((visible, 0)),
            input[1],
        );
        if let Some(choice) = form.confirm {
            let choices = ["Сохранить (s)", "Продолжить (c)", "Отбросить (d)"];
            let text = choices
                .iter()
                .enumerate()
                .map(|(i, s)| format!("{} {}", if i == choice { ">" } else { " " }, s))
                .collect::<Vec<_>>()
                .join("\n");
            overlay(frame, area, &text, "Несохранённый ввод · ←/→ Enter");
        }
    }
    if let Some(picker) = &ui.picker {
        frame.render_widget(Clear, rows[1]);
        let items: Vec<_> = picker
            .candidates
            .iter()
            .map(|(id, title)| ListItem::new(safe(&format!("{title} · {id}"))))
            .collect();
        let mut state = ListState::default().with_selected(Some(picker.selected));
        frame.render_stateful_widget(
            List::new(items)
                .block(panel(&format!(
                    "Связь: / {}{} · {} · ↑/↓ Enter · Esc без связи",
                    safe(&picker.query),
                    if picker.editing { " [ВВОД]" } else { "" },
                    if picker.loading {
                        "Поиск…"
                    } else {
                        "опубликованная заметка"
                    }
                )))
                .highlight_symbol("> "),
            rows[1],
            &mut state,
        );
    }
    if ui.help {
        overlay(
            frame,
            area,
            "↑/↓ j/k — выбор / прокрутка текста; / — полнотекстовый поиск\nВ выборе связи / — ввод fuzzy запроса, Esc — назад\nTab — список/текст; i/Esc — Library/Inbox; r — сверить файлы\nc — capture: многострочный текст, Tab заголовок\nn — title/body → сохранить draft → связь → publish\nCtrl-S / F2 / Ctrl-Enter — сохранить; Enter в тексте — строка\ne — внешний редактор; l — связь; p — публикация\nEsc в форме: сохранить / продолжить / отбросить\nq — выход; Ctrl-C — сохранить принятый ввод и выйти 130\nEsc/?/q — закрыть справку",
            "Справка",
        );
    }
}
fn overlay(frame: &mut Frame, area: Rect, text: &str, title: &str) {
    let rect = Rect::new(
        area.x + 2,
        area.y + 2,
        area.width.saturating_sub(4),
        area.height.saturating_sub(4),
    );
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(text)
            .block(panel(title))
            .wrap(Wrap { trim: false }),
        rect,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app() -> (tempfile::TempDir, App) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault");
        App::init(&path, "Корень").unwrap();
        let app = App::open(&path).unwrap();
        (dir, app)
    }
    fn outcome(path: &str) -> Data {
        Data::Outcome(Outcome::ok(serde_json::json!({"items": [{"path": path}]})))
    }
    #[test]
    fn stale_search_preview_and_picker_replies_never_replace_current_state() {
        let (_dir, app) = app();
        let mut worker = Worker::start(app);
        let mut ui = Ui::new();
        ui.search = Some(Search {
            query: "new".into(),
            ..Search::default()
        });
        let old = worker.read(Read::Search("old".into()));
        let new = worker.read(Read::Search("new".into()));
        ui.reply(
            &mut worker,
            Reply {
                id: old,
                lane: Some(Lane::Search),
                result: Ok(outcome("old.md")),
            },
            FreshnessMode::Stat,
        );
        assert!(ui.search.as_ref().unwrap().paths.is_empty());
        ui.reply(
            &mut worker,
            Reply {
                id: new,
                lane: Some(Lane::Search),
                result: Ok(outcome("new.md")),
            },
            FreshnessMode::Stat,
        );
        assert_eq!(ui.search.as_ref().unwrap().paths, ["new.md"]);
        let old = worker.read(Read::Preview("old.md".into()));
        worker.read(Read::Preview("new.md".into()));
        ui.body_path = Some("new.md".into());
        ui.reply(
            &mut worker,
            Reply {
                id: old,
                lane: Some(Lane::Preview),
                result: Ok(Data::Preview(Record::from_text(
                    "old.md".into(),
                    "old body".into(),
                ))),
            },
            FreshnessMode::Stat,
        );
        assert!(ui.body.is_none());
        ui.pick(&mut worker, "source".into(), false);
        let old = worker.read(Read::Fuzzy("old".into()));
        ui.picker.as_mut().unwrap().query = "new".into();
        ui.fuzzy(&mut worker);
        ui.reply(
            &mut worker,
            Reply {
                id: old,
                lane: Some(Lane::Fuzzy),
                result: Ok(Data::Outcome(Outcome::ok(
                    serde_json::json!({"items": [{"id": "old", "title": "old"}]}),
                ))),
            },
            FreshnessMode::Stat,
        );
        assert!(ui.picker.as_ref().unwrap().loading);
        assert!(ui.picker.as_ref().unwrap().candidates.is_empty());
    }
    #[test]
    fn search_retains_relevance_and_navigation_survives_pending_catalog() {
        let (_dir, app) = app();
        let mut worker = Worker::start(app);
        let mut ui = Ui::new();
        ui.catalog.records = vec![
            Record::from_text("a.md".into(), "a".into()),
            Record::from_text("b.md".into(), "b".into()),
        ];
        ui.by_path = [("a.md".into(), 0), ("b.md".into(), 1)].into();
        ui.search = Some(Search {
            query: "query".into(),
            paths: vec!["b.md".into(), "a.md".into()],
            ..Search::default()
        });
        assert_eq!(
            ui.records()
                .iter()
                .map(|r| r.path.as_str())
                .collect::<Vec<_>>(),
            ["b.md", "a.md"]
        );
        ui.refresh(&mut worker, FreshnessMode::Full, None);
        ui.selected = 1;
        ui.preview(&mut worker, false);
        assert!(ui.catalog_loading);
        assert_eq!(ui.current().unwrap().path, "a.md");
        let id = worker.read(Read::Catalog(FreshnessMode::Full));
        let mut catalog = ui.catalog.clone();
        catalog.freshness.generation = Some(1);
        ui.catalog.freshness.generation = Some(1);
        ui.body = Some(Record::from_text("a.md".into(), "preview retained".into()));
        ui.body_loading = false;
        ui.reply(
            &mut worker,
            Reply {
                id,
                lane: Some(Lane::Catalog),
                result: Ok(Data::Catalog(catalog)),
            },
            FreshnessMode::Stat,
        );
        assert_eq!(ui.current().unwrap().path, "a.md");
        assert_eq!(ui.body.as_ref().unwrap().note.body, "preview retained");
        assert!(!ui.body_loading);
    }
    #[test]
    fn terminal_error_drains_accepted_save_once_and_preserves_original() {
        let (_dir, app) = app();
        let original = app.scan().unwrap().records[0].clone();
        let original_bytes = app.vault.read(&original.path).unwrap();
        let mut worker = Worker::start(app.clone());
        let mut ui = Ui::new();
        ui.form = Some(Form {
            body: "accepted once 🌍".into(),
            ..Form::default()
        });
        ui.save(&mut worker, false).unwrap();
        assert!(ui.form.is_some(), "До ack форма остаётся в UI");
        ui.exiting(&mut worker, Exit::Normal);
        assert!(ui.finished().is_none(), "q ждёт принятую запись");
        let mut error = Error::new("io", "terminal failed");
        preserve(&mut ui, &mut worker, &app, &mut error);
        assert!(ui.form.is_none());
        assert!(error.saved);
        assert!(
            error.data.as_ref().unwrap()["path"]
                .as_str()
                .unwrap()
                .starts_with("inbox/")
        );
        let catalog = app.scan().unwrap();
        assert_eq!(
            catalog
                .records
                .iter()
                .filter(|r| r.note.body == "accepted once 🌍")
                .count(),
            1
        );
        assert_eq!(app.vault.read(&original.path).unwrap(), original_bytes);
    }
    #[test]
    fn catalog_error_keeps_confirmed_save_path_and_index_warning() {
        let (_dir, app) = app();
        let mut worker = Worker::start(app);
        let mut ui = Ui::new();
        let mut saved = Outcome::ok(serde_json::json!({"id":"saved-id", "path":"inbox/saved.md"}));
        saved.saved = true;
        saved.warnings.push(crate::app::Warning {
            code: "index_stale".into(),
            message: "Файл сохранён".into(),
            path: None,
        });
        ui.last_saved = Some(saved);
        let id = worker.read(Read::Catalog(FreshnessMode::Stat));
        ui.reply(
            &mut worker,
            Reply {
                id,
                lane: Some(Lane::Catalog),
                result: Err(Error::new("index_unavailable", "broken sqlite")),
            },
            FreshnessMode::Stat,
        );
        assert!(ui.message.contains("Сохранено saved-id inbox/saved.md"));
        assert!(ui.message.contains("index_stale"));
        assert!(ui.message.contains("index_unavailable"));
    }
    #[test]
    fn unchanged_reconciliation_does_not_require_a_frame() {
        let mut a = Ui::new().catalog;
        a.freshness.generation = Some(7);
        a.freshness.mode = FreshnessMode::Stat;
        let mut b = a.clone();
        b.freshness.mode = FreshnessMode::Full;
        assert!(same_catalog(&a, &b));
        b.freshness.mode = FreshnessMode::Cached;
        assert!(!same_catalog(&a, &b));
        b.freshness.mode = FreshnessMode::Full;
        b.warnings.push(crate::app::Warning {
            code: "index_stale".into(),
            message: "Изменилось состояние".into(),
            path: None,
        });
        assert!(!same_catalog(&a, &b));
        b.warnings.clear();
        b.freshness.generation = Some(8);
        assert!(!same_catalog(&a, &b));
    }
    #[test]
    fn input_keeps_unicode_and_literal_navigation() {
        let mut form = Form::default();
        form.insert("jknp? e\u{301}👨‍👩‍👧‍👦\r\n第二行");
        assert_eq!(form.body, "jknp? e\u{301}👨‍👩‍👧‍👦\n第二行");
        let mut text = "e\u{301}👨‍👩‍👧‍👦".to_owned();
        backspace(&mut text);
        assert_eq!(text, "e\u{301}");
        backspace(&mut text);
        assert_eq!(text, "");
        assert!(
            !safe("\x1b]0;danger\x07\u{9b}31m")
                .chars()
                .any(|c| c.is_control())
        );
    }
}
