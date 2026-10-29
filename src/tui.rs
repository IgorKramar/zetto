use crate::app::{App, Catalog, Outcome, Record, Stage};
use crate::error::{Error, Result};
use crate::safe;
use crate::{editor, interrupt};
use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use std::io;
use std::time::Duration;
use unicode_segmentation::UnicodeSegmentation;

// Drop и hook используют одну идемпотентную последовательность, в том числе при panic.
fn restore() {
    let _ = terminal::disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        DisableBracketedPaste,
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
}
struct Ui {
    catalog: Catalog,
    inbox: bool,
    selected: usize,
    preview: bool,
    scroll: u16,
    form: Option<Form>,
    picker: Option<Picker>,
    help: bool,
    message: String,
}
impl Ui {
    fn records(&self) -> Vec<&Record> {
        self.catalog
            .records
            .iter()
            .filter(|r| (r.stage == Stage::Inbox) == self.inbox)
            .collect()
    }
    fn current(&self) -> Option<&Record> {
        self.records().get(self.selected).copied()
    }
    fn refresh(&mut self, app: &App, selected: Option<&str>) -> Result<()> {
        let path = self.current().map(|r| r.path.clone());
        self.catalog = app.scan()?;
        let records = self.records();
        self.selected = records
            .iter()
            .position(|r| {
                selected.is_some_and(|id| r.id() == Some(id))
                    || (selected.is_none() && path.as_deref() == Some(&r.path))
            })
            .unwrap_or(self.selected.min(records.len().saturating_sub(1)));
        Ok(())
    }
    fn report(&mut self, result: Result<Outcome>) {
        self.message = match result {
            Ok(out) => format!(
                "Сохранено {} {}{}",
                out.data["id"].as_str().unwrap_or(""),
                out.data["path"].as_str().unwrap_or(""),
                diagnostics(&out.diagnostics)
            ),
            Err(e) => error_text(&e),
        };
    }
    // После подтверждённого capture форму удаляем только после сохранения ID/пути.
    fn save(&mut self, app: &App, new_flow: bool) -> Result<Option<Outcome>> {
        let Some(form) = self.form.as_ref() else {
            return Ok(None);
        };
        if !form.pending() {
            self.form = None;
            return Ok(None);
        }
        let out = app.capture(&form.title, &form.body)?;
        let id = out.data["id"].as_str().unwrap().to_owned();
        self.form = None;
        self.inbox = true;
        self.report(Ok(out.clone()));
        if let Err(e) = self.refresh(app, Some(&id)) {
            self.message
                .push_str(&format!("; файлы сохранены, сверка: {}", error_text(&e)));
        }
        if new_flow && let Err(e) = self.pick(app, &id, true) {
            self.message
                .push_str(&format!("; выбор связи: {}", error_text(&e)));
        }
        Ok(Some(out))
    }
    fn interrupted(&mut self, app: &App) -> Result<Error> {
        let saved = self.save(app, false)?;
        let mut error = Error::new(
            "interrupted",
            if saved.is_some() {
                "Прервано; принятый ввод сохранён"
            } else {
                "Прервано пользователем"
            },
        );
        if let Some(out) = saved {
            saved_error(&mut error, out);
        }
        Ok(error)
    }
    fn pick(&mut self, app: &App, source: &str, publish: bool) -> Result<()> {
        self.refresh(app, Some(source))?;
        let candidates = self
            .catalog
            .records
            .iter()
            .filter(|r| r.stage == Stage::Published)
            .filter_map(|r| {
                r.id().filter(|id| *id != source).map(|id| {
                    (
                        id.to_owned(),
                        r.note.title().unwrap_or("Без заголовка").to_owned(),
                    )
                })
            })
            .collect();
        self.picker = Some(Picker {
            source: source.to_owned(),
            candidates,
            selected: 0,
            publish,
        });
        Ok(())
    }
    fn publish(&mut self, app: &App, id: &str, target: Option<&str>) {
        let result = app.publish(id, target);
        if result.is_ok() {
            self.inbox = false;
        }
        self.report(result);
        if let Err(e) = self.refresh(app, Some(id)) {
            self.message
                .push_str(&format!("; сверка: {}", error_text(&e)));
        }
    }
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
    let mut ui = Ui {
        catalog: app.scan()?,
        inbox: false,
        selected: 0,
        preview: false,
        scroll: 0,
        form: None,
        picker: None,
        help: false,
        message: "c — записать мысль · r — сверить файлы".into(),
    };
    // Устанавливаем один раз на весь сеанс; edit не меняет hook.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));
    let session = Session;
    session.resume()?;
    let mut screen = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    screen.clear()?;
    let result = (|| {
        let mut dirty = true;
        loop {
            if interrupt::requested() {
                match ui.interrupted(&app) {
                    Ok(error) => return Err(error),
                    Err(e) => {
                        interrupt::clear();
                        ui.message = format!(
                            "{}\nТекст НЕ подтверждён, форма остаётся открытой",
                            error_text(&e)
                        );
                        dirty = true;
                    }
                }
            }
            if dirty {
                screen.draw(|frame| draw(frame, &ui, color))?;
                dirty = false;
            }
            if !event::poll(Duration::from_millis(100))? {
                continue;
            }
            let event = event::read()?;
            dirty = true;
            match event {
                Event::Resize(_, _) => {
                    screen.clear()?;
                }
                Event::Paste(text) => {
                    if let Some(form) = ui.form.as_mut() {
                        form.insert(&text);
                    }
                }
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        match ui.interrupted(&app) {
                            Ok(error) => return Err(error),
                            Err(e) => {
                                interrupt::clear();
                                ui.message = format!(
                                    "{}\nТекст НЕ подтверждён, форма остаётся открытой",
                                    error_text(&e)
                                );
                            }
                        }
                        continue;
                    }
                    let size = screen.size()?;
                    if size.width < 60 || size.height < 12 {
                        if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                            match ui.save(&app, false) {
                                Ok(_) => return Ok(()),
                                Err(e) => ui.message = error_text(&e),
                            }
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
                                    if let Err(e) = ui.save(&app, false) {
                                        ui.message = error_text(&e);
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
                            if let Err(e) = ui.save(&app, publish) {
                                ui.message = error_text(&e);
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
                        match key.code {
                            KeyCode::Up | KeyCode::Char('k') => {
                                picker.selected = picker.selected.saturating_sub(1)
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                picker.selected = (picker.selected + 1)
                                    .min(picker.candidates.len().saturating_sub(1))
                            }
                            KeyCode::Enter | KeyCode::Esc => {
                                let picker = ui.picker.take().unwrap();
                                let target = if key.code == KeyCode::Enter {
                                    picker
                                        .candidates
                                        .get(picker.selected)
                                        .map(|(id, _)| id.as_str())
                                } else {
                                    None
                                };
                                if picker.publish {
                                    ui.publish(&app, &picker.source, target);
                                } else if let Some(target) = target {
                                    ui.report(app.link(&picker.source, target, None));
                                    if let Err(e) = ui.refresh(&app, Some(&picker.source)) {
                                        ui.message = error_text(&e);
                                    }
                                }
                            }
                            _ => {}
                        }
                        continue;
                    }
                    match key.code {
                        KeyCode::Char('q') => return Ok(()),
                        KeyCode::Char('?') => ui.help = true,
                        KeyCode::Char('i') | KeyCode::Esc => {
                            ui.inbox = !ui.inbox;
                            ui.selected = 0;
                            ui.scroll = 0;
                        }
                        KeyCode::Tab | KeyCode::BackTab => ui.preview = !ui.preview,
                        KeyCode::Char('c') => ui.form = Some(Form::default()),
                        KeyCode::Char('n') => {
                            ui.form = Some(Form {
                                title_focus: true,
                                publish: true,
                                ..Form::default()
                            })
                        }
                        KeyCode::Char('r') => match ui.refresh(&app, None) {
                            Ok(()) => ui.message = "Файлы сверены".into(),
                            Err(e) => ui.message = error_text(&e),
                        },
                        KeyCode::Down | KeyCode::Char('j') => {
                            if ui.preview {
                                ui.scroll = ui.scroll.saturating_add(1);
                            } else {
                                ui.selected =
                                    (ui.selected + 1).min(ui.records().len().saturating_sub(1));
                                ui.scroll = 0;
                            }
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            if ui.preview {
                                ui.scroll = ui.scroll.saturating_sub(1);
                            } else {
                                ui.selected = ui.selected.saturating_sub(1);
                                ui.scroll = 0;
                            }
                        }
                        KeyCode::Char('l') | KeyCode::Char('p') | KeyCode::Char('e') => {
                            if let Some(id) = ui.current().and_then(Record::id).map(str::to_owned) {
                                match key.code {
                                    KeyCode::Char('l') => {
                                        if let Err(e) = ui.pick(&app, &id, false) {
                                            ui.message = error_text(&e);
                                        }
                                    }
                                    KeyCode::Char('p') => ui.publish(&app, &id, None),
                                    _ => {
                                        let result = editor::command().and_then(|args| {
                                            restore();
                                            let edited = app.edit(&id, &args);
                                            session.resume()?;
                                            screen.clear()?;
                                            edited
                                        });
                                        ui.report(result);
                                        if let Err(e) = ui.refresh(&app, Some(&id)) {
                                            ui.message
                                                .push_str(&format!("\nСверка: {}", error_text(&e)));
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
                _ => {
                    dirty = false;
                }
            }
        }
    })();
    // Ошибка терминала тоже не должна обойти capture принятой формы.
    if let Err(mut error) = result {
        match ui.save(&app, false) {
            Ok(Some(out)) => saved_error(&mut error, out),
            Ok(None) => {}
            Err(save_error) => {
                error.message.push_str(&format!(
                    "; текст НЕ подтверждён: {}",
                    error_text(&save_error)
                ));
                if let Some(form) = &ui.form {
                    error.data = Some(Box::new(
                        serde_json::json!({"unsaved_title": form.title, "unsaved_body": form.body}),
                    ));
                }
            }
        }
        return Err(error);
    }
    result
}
fn saved_error(error: &mut Error, out: Outcome) {
    error.saved = out.saved;
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
            "zetto / {} · {} · {}",
            if ui.inbox { "Inbox" } else { "Library" },
            if ui.preview {
                "[ТЕКСТ]"
            } else {
                "[СПИСОК]"
            },
            ui.records().len()
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
                        r.note.body
                    )
                })
                .unwrap_or_else(|| "Нет заметок · c — записать мысль".into());
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
        Paragraph::new("i Inbox/Library · Tab фокус · c ввод · n новая · e l p · r сверка · ? · q"),
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
                .block(panel(
                    "Связь: опубликованная заметка · ↑/↓ Enter · Esc без связи",
                ))
                .highlight_symbol("> "),
            rows[1],
            &mut state,
        );
    }
    if ui.help {
        overlay(
            frame,
            area,
            "↑/↓ j/k — выбор / прокрутка текста\nTab — список/текст; i/Esc — Library/Inbox; r — сверить файлы\nc — capture: многострочный текст, Tab заголовок\nn — title/body → сохранить draft → связь → publish\nCtrl-S / F2 / Ctrl-Enter — сохранить; Enter в тексте — строка\ne — внешний редактор; l — связь; p — публикация\nEsc в форме: сохранить / продолжить / отбросить\nq — выход; Ctrl-C — сохранить принятый ввод и выйти 130\nEsc/?/q — закрыть справку",
            "Справка v0.1",
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
