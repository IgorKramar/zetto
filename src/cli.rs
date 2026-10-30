use crate::app::{App, LinkDirection, Outcome, SearchMode, StageFilter};
use crate::error::{Error, Result};
use crate::index::{Cancellation, FreshnessMode};
use crate::safe;
use crate::vault::RecoveryMode;
use crate::{config, editor, format, interrupt};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

const MAX_BODY: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum OutputFormat {
    #[default]
    Table,
    Json,
    Jsonl,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum Color {
    #[default]
    Auto,
    Always,
    Never,
}
#[derive(Clone, Copy, Debug, ValueEnum)]
enum Stage {
    Published,
    Inbox,
    All,
}
#[derive(Clone, Copy, Debug, ValueEnum)]
enum Direction {
    Incoming,
    Outgoing,
    Both,
}
#[derive(Subcommand)]
enum IndexCommand {
    Rebuild,
}
#[derive(Parser)]
#[command(name = "zetto", version, about = "Связанная база знаний в Markdown", color = clap::ColorChoice::Never)]
struct Cli {
    #[arg(long, global = true)]
    vault: Option<PathBuf>,
    #[arg(long, global = true, value_enum, default_value = "table")]
    format: OutputFormat,
    #[arg(long, global = true, value_enum, default_value = "auto")]
    color: Color,
    #[arg(long, global = true)]
    no_interactive: bool,
    #[arg(long, global = true, conflicts_with = "cached")]
    fresh: bool,
    #[arg(long, global = true)]
    cached: bool,
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Args)]
struct Input {
    #[arg(conflicts_with_all = ["stdin", "file"])]
    text: Option<String>,
    #[arg(long, conflicts_with = "file")]
    stdin: bool,
    #[arg(long)]
    file: Option<PathBuf>,
    #[arg(long, default_value = "")]
    title: String,
}
#[derive(Subcommand)]
enum Command {
    Init {
        path: Option<PathBuf>,
        #[arg(long, default_value = "Корень")]
        root_title: String,
    },
    Capture(Input),
    New {
        #[command(flatten)]
        input: Input,
        #[arg(long, value_parser = id)]
        link: Option<String>,
    },
    Show {
        #[arg(value_parser = id)]
        id: String,
        #[arg(long)]
        raw: bool,
    },
    List {
        #[arg(long, value_enum, default_value = "published")]
        stage: Stage,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=10_000))]
        limit: u32,
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
    Search {
        query: String,
        #[arg(long, conflicts_with_all = ["regex", "fts"])]
        literal: bool,
        #[arg(long, conflicts_with = "fts")]
        regex: bool,
        #[arg(long)]
        fts: bool,
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=10_000))]
        limit: u32,
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
    Links {
        #[arg(value_parser = id)]
        id: String,
        #[arg(long, value_enum, default_value = "both")]
        direction: Direction,
    },
    Graph {
        #[arg(value_parser = id)]
        id: String,
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=3))]
        depth: u32,
    },
    Tags,
    Path {
        #[arg(required = true, num_args = 1.., value_parser = id)]
        ids: Vec<String>,
        #[arg(long)]
        null: bool,
    },
    Index {
        #[command(subcommand)]
        command: IndexCommand,
    },
    Edit {
        #[arg(value_parser = id)]
        id: String,
    },
    Link {
        #[arg(value_parser = id)]
        source: String,
        #[arg(value_parser = id)]
        target: String,
        #[arg(long)]
        display: Option<String>,
    },
    Publish {
        #[arg(value_parser = id)]
        id: String,
        #[arg(long, value_parser = id)]
        link: Option<String>,
    },
    Doctor {
        #[arg(long, value_parser = id)]
        operation: Option<String>,
        #[arg(long, conflicts_with = "restore", requires = "operation")]
        resume: bool,
        #[arg(long, requires = "operation")]
        restore: bool,
    },
    Tui,
}
fn id(value: &str) -> std::result::Result<String, String> {
    if format::valid_id(value) {
        Ok(value.to_owned())
    } else {
        Err("Нужен полный канонический ULID".into())
    }
}

/// Сериализует ошибки разбора в формат, указанный даже после неверного аргумента.
fn requested_format(args: &[OsString]) -> OutputFormat {
    let mut format = OutputFormat::Table;
    let mut args = args.iter().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        let value = if arg == "--format" {
            args.next().and_then(|v| v.to_str())
        } else {
            arg.to_str().and_then(|s| s.strip_prefix("--format="))
        };
        if let Some(value) = value {
            format = match value {
                "json" => OutputFormat::Json,
                "jsonl" => OutputFormat::Jsonl,
                _ => OutputFormat::Table,
            };
        }
    }
    format
}

pub fn run() -> u8 {
    let args: Vec<_> = std::env::args_os().collect();
    let selected = requested_format(&args);
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => {
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) {
                return if write_stdout(error.to_string().as_bytes()).is_ok() {
                    0
                } else {
                    4
                };
            }
            return respond(Err(Error::new("usage", error.to_string())), selected, false);
        }
    };
    let color = cli.format == OutputFormat::Table
        && match cli.color {
            Color::Always => true,
            Color::Never => false,
            Color::Auto => io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
        };
    let result = interrupt::install().and_then(|()| execute(&cli, color));
    match result {
        Ok(Response::Raw(bytes)) => {
            if interrupt::requested() {
                130
            } else if write_stdout(&bytes).is_ok() {
                0
            } else {
                4
            }
        }
        Ok(Response::Paths(out, null)) => match finish_interrupt(Ok(out)) {
            Ok(out) => {
                if write_paths(&out, null).is_ok() {
                    0
                } else {
                    4
                }
            }
            Err(mut error) => {
                error.data = None;
                respond(Err(error), cli.format, color)
            }
        },
        Ok(Response::Outcome(out)) => respond(finish_interrupt(Ok(out)), cli.format, color),
        Err(error) => respond(finish_interrupt(Err(error)), cli.format, color),
    }
}

enum Response {
    Outcome(Outcome),
    Raw(Vec<u8>),
    Paths(Outcome, bool),
}
fn execute(cli: &Cli, color: bool) -> Result<Response> {
    let tty = io::stdin().is_terminal() && io::stdout().is_terminal();
    let command = match cli.command.as_ref() {
        Some(command) => command,
        None if tty && !cli.no_interactive && cli.format == OutputFormat::Table => &Command::Tui,
        None => {
            return Err(Error::new(
                "usage",
                "Использование: zetto [OPTIONS] COMMAND; справка: zetto --help",
            ));
        }
    };
    let freshness = if cli.cached {
        FreshnessMode::Cached
    } else if cli.fresh {
        FreshnessMode::Full
    } else {
        FreshnessMode::Stat
    };
    let interactive = tty && !cli.no_interactive && cli.format == OutputFormat::Table;
    if matches!(command, Command::Tui) {
        if !interactive {
            return Err(Error::new(
                "interaction_required",
                "TUI требует stdin/stdout TTY, --format table и разрешённый интерактивный режим",
            ));
        }
        let path = config::discover(cli.vault.as_deref())?;
        crate::tui::run(App::open(&path)?.with_freshness(freshness), color)?;
        return Ok(Response::Raw(Vec::new()));
    }
    if matches!(command, Command::Edit { .. }) && (!tty || cli.no_interactive) {
        return Err(Error::new(
            "interaction_required",
            "edit требует stdin/stdout TTY и разрешённый редактор",
        ));
    }
    if matches!(command, Command::Show { raw: true, .. }) && cli.format != OutputFormat::Table {
        return Err(Error::new("usage", "--raw несовместим с JSON/JSONL"));
    }
    if matches!(command, Command::Path { null: true, .. }) && cli.format != OutputFormat::Table {
        return Err(Error::new("usage", "--null несовместим с JSON/JSONL"));
    }
    if let Command::Init { path, root_title } = command {
        if path.is_some() && cli.vault.is_some() {
            return Err(Error::new(
                "usage",
                "init PATH и --vault нельзя указывать вместе",
            ));
        }
        check_interrupt()?;
        let path = config::discover(path.as_deref().or(cli.vault.as_deref()))?;
        return App::init(&path, root_title).map(Response::Outcome);
    }
    if let Command::Capture(input) | Command::New { input, .. } = command {
        let body = input_body(input, interactive)?;
        let path = config::discover(cli.vault.as_deref())?;
        let app = App::open(&path)?;
        // Принятые из stdin байты сохраняем и после SIGINT; публикация уже запрещена.
        let draft = app.capture(&input.title, body.as_deref().unwrap_or(""))?;
        let id = draft.data["id"].as_str().unwrap();
        let mut recovery = Vec::new();
        let mut warnings = draft.warnings.clone();
        let operation = (|| {
            check_interrupt()?;
            if body.is_none() {
                eprintln!("Черновик {} сохранён. Открываю редактор.", safe(id));
                let edited = app.edit(id, &editor::command()?)?;
                recovery.extend(edited.recovery.clone());
                check_interrupt_with(&edited)?;
                warnings.extend(edited.warnings.clone());
            }
            if let Command::New { link, .. } = command {
                let chosen = if body.is_none() && link.is_none() {
                    eprint!("Полный ID опубликованной заметки для связи (пусто — без связи): ");
                    io::stderr().flush()?;
                    Some(read_input(true)?.trim().to_owned()).filter(|s| !s.is_empty())
                } else {
                    link.clone()
                };
                check_interrupt()?;
                app.publish(id, chosen.as_deref())
            } else {
                let mut out = draft.clone();
                out.warnings.clear(); // Предупреждения этапов объединяются ниже один раз.
                Ok(out)
            }
        })();
        return operation
            .map(|mut out| {
                out.recovery.extend(recovery.clone());
                out.warnings.extend(warnings.clone());
                Response::Outcome(out)
            })
            .map_err(|mut e| {
                e.saved = true;
                if e.data.is_none() {
                    e.data = Some(Box::new(draft.data));
                }
                e.recovery.extend(recovery);
                e.context.warnings.extend(warnings);
                e
            });
    }
    check_interrupt()?;
    let path = config::discover(cli.vault.as_deref())?;
    if let Command::Doctor {
        operation,
        resume,
        restore,
    } = command
    {
        let mode = if *resume {
            Some(RecoveryMode::Resume)
        } else if *restore {
            Some(RecoveryMode::Restore)
        } else {
            None
        };
        return App::doctor_at(&path, operation.as_deref(), mode).map(Response::Outcome);
    }
    let app = App::open(&path)?.with_freshness(freshness);
    let token = Cancellation::default();
    let outcome = match command {
        Command::Show { id, raw: true } => return app.raw(id).map(Response::Raw),
        Command::Show { id, .. } => app.show(id),
        Command::List {
            stage,
            tag,
            limit,
            offset,
        } => app.list(
            match stage {
                Stage::Published => StageFilter::Published,
                Stage::Inbox => StageFilter::Inbox,
                Stage::All => StageFilter::All,
            },
            tag.as_deref(),
            *limit as usize,
            *offset,
        ),
        Command::Search {
            query,
            literal,
            regex,
            fts,
            limit,
            offset,
        } => app.search(
            query,
            if *literal {
                SearchMode::Literal
            } else if *regex {
                SearchMode::Regex
            } else if *fts {
                SearchMode::Fts
            } else {
                SearchMode::Default
            },
            *limit as usize,
            *offset,
            &token,
        ),
        Command::Links { id, direction } => app.links(
            id,
            match direction {
                Direction::Incoming => LinkDirection::Incoming,
                Direction::Outgoing => LinkDirection::Outgoing,
                Direction::Both => LinkDirection::Both,
            },
            &token,
        ),
        Command::Graph { id, depth } => app.graph(id, *depth as usize, &token),
        Command::Tags => app.tags(&token),
        Command::Path { ids, null } => {
            let out = app.paths(ids, &token)?;
            return Ok(if cli.format == OutputFormat::Table {
                Response::Paths(out, *null)
            } else {
                Response::Outcome(out)
            });
        }
        Command::Index {
            command: IndexCommand::Rebuild,
        } => app.rebuild_index(&token),
        Command::Edit { id } => app.edit(id, &editor::command()?),
        Command::Link {
            source,
            target,
            display,
        } => app.link(source, target, display.as_deref()),
        Command::Publish { id, link } => app.publish(id, link.as_deref()),
        _ => unreachable!(),
    }?;
    Ok(Response::Outcome(outcome))
}

fn input_body(input: &Input, interactive: bool) -> Result<Option<String>> {
    if let Some(text) = &input.text {
        checked_body(text.as_bytes().to_vec()).map(Some)
    } else if input.stdin {
        read_input(false).map(Some)
    } else if let Some(path) = &input.file {
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(MAX_BODY as u64 + 1)
            .read_to_end(&mut bytes)?;
        checked_body(bytes).map(Some)
    } else if interactive {
        Ok(None)
    } else {
        Err(Error::new(
            "interaction_required",
            "Укажите TEXT, --stdin или --file; редактор требует TTY",
        ))
    }
}
fn checked_body(bytes: Vec<u8>) -> Result<String> {
    if bytes.len() > MAX_BODY {
        return Err(Error::new("validation_failed", "Тело превышает 16 MiB"));
    }
    String::from_utf8(bytes).map_err(|_| Error::new("validation_failed", "Ввод должен быть UTF-8"))
}
fn read_input(line: bool) -> Result<String> {
    // Ограниченный канал даёт отменить ожидание pipe/строки без unsafe и async.
    let (sender, receiver) = mpsc::sync_channel(4);
    std::thread::spawn(move || {
        let mut input = io::stdin().lock();
        loop {
            let mut bytes = vec![0; if line { 1 } else { 8192 }];
            match input.read(&mut bytes) {
                Ok(0) => break,
                Ok(n) => {
                    bytes.truncate(n);
                    let done = line && bytes.last() == Some(&b'\n');
                    if sender.send(Ok(bytes)).is_err() || done {
                        break;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    let _ = sender.send(Err(e));
                    break;
                }
            }
        }
    });
    let mut bytes = Vec::new();
    loop {
        match receiver.recv_timeout(Duration::from_millis(40)) {
            Ok(chunk) => bytes.extend(chunk?),
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
        if bytes.len() > MAX_BODY {
            break;
        }
        if interrupt::requested() {
            for chunk in receiver.try_iter() {
                bytes.extend(chunk?);
            }
            // Последовательность UTF-8, ещё не принятая целиком, не стала текстом.
            if let Err(error) = std::str::from_utf8(&bytes)
                && error.error_len().is_none()
            {
                bytes.truncate(error.valid_up_to());
            }
            break;
        }
    }
    checked_body(bytes)
}
fn check_interrupt() -> Result<()> {
    if interrupt::requested() {
        Err(Error::new("interrupted", "Прервано пользователем"))
    } else {
        Ok(())
    }
}
fn check_interrupt_with(out: &Outcome) -> Result<()> {
    finish_interrupt(Ok(out.clone())).map(|_| ())
}
fn finish_interrupt(result: Result<Outcome>) -> Result<Outcome> {
    if !interrupt::requested() {
        return result;
    }
    let mut error = Error::new(
        "interrupted",
        "Прервано пользователем; подтверждённые записи сохранены",
    );
    match result {
        Ok(out) => {
            error.saved = out.saved;
            error.data = Some(Box::new(out.data));
            error.recovery = out.recovery;
            error.diagnostics = out.diagnostics;
            error.context.warnings = out.warnings;
            error.context.freshness = out.freshness;
        }
        Err(previous) => {
            error.saved = previous.saved;
            error.data = previous.data;
            error.recovery = previous.recovery;
            error.diagnostics = previous.diagnostics;
            error.context = previous.context;
        }
    }
    Err(error)
}

fn respond(result: Result<Outcome>, format: OutputFormat, color: bool) -> u8 {
    let (out, error, exit) = match result {
        Ok(out) => (out, Value::Null, 0),
        Err(error) => {
            let mut out = Outcome::ok(error.data.map(|d| *d).unwrap_or(Value::Null));
            out.status = if error.saved { "draft_saved" } else { "error" }.into();
            out.saved = error.saved;
            out.diagnostics = error.diagnostics;
            out.recovery = error.recovery;
            out.warnings = error.context.warnings;
            out.freshness = error.context.freshness;
            (
                out,
                json!({"code":error.code,"message":error.message}),
                error.exit,
            )
        }
    };
    let envelope = json!({
        "schema_version":1,"status":out.status,"saved":out.saved,"data":out.data,"error":error,
        "warnings":out.warnings,"diagnostics":out.diagnostics,"recovery":out.recovery,
        "freshness":out.freshness,"complete":out.complete,"truncated":out.truncated,
    });
    let written = match format {
        OutputFormat::Json => write_stdout(format!("{envelope}\n").as_bytes()),
        OutputFormat::Jsonl => write_jsonl(&envelope),
        OutputFormat::Table => human(&envelope, color),
    };
    if written.is_err() { 4 } else { exit }
}
fn write_stdout(bytes: &[u8]) -> io::Result<()> {
    let mut out = io::stdout().lock();
    out.write_all(bytes)?;
    out.flush()
}
fn write_paths(outcome: &Outcome, null: bool) -> io::Result<()> {
    human_notices(
        &json!({"warnings":outcome.warnings,"diagnostics":outcome.diagnostics,"recovery":outcome.recovery,"freshness":outcome.freshness}),
    )?;
    let mut out = io::stdout().lock();
    for item in outcome.data["items"].as_array().unwrap() {
        out.write_all(item["path"].as_str().unwrap().as_bytes())?;
        out.write_all(if null { b"\0" } else { b"\n" })?;
    }
    out.flush()
}

fn write_jsonl(envelope: &Value) -> io::Result<()> {
    let mut out = io::stdout().lock();
    let mut count = 0;
    let mut row = |kind: &str, data: &Value| -> io::Result<()> {
        writeln!(
            out,
            "{}",
            json!({"schema_version":1,"type":kind,"data":data})
        )
    };
    let data = &envelope["data"];
    if let Some(items) = data.get("items").and_then(Value::as_array) {
        for item in items {
            row("item", item)?;
            count += 1;
        }
    } else if !data.is_null() {
        row("item", data)?;
        count = 1;
    }
    for warning in envelope["warnings"].as_array().unwrap() {
        row("warning", warning)?;
    }
    for diagnostic in envelope["diagnostics"].as_array().unwrap() {
        row("diagnostic", diagnostic)?;
    }
    let mut summary = envelope.clone();
    let object = summary.as_object_mut().unwrap();
    for key in ["schema_version", "data", "warnings", "diagnostics"] {
        object.remove(key);
    }
    object.insert("count".into(), json!(count));
    if let Some(offset) = data.get("next_offset") {
        object.insert("next_offset".into(), offset.clone());
    }
    row("summary", &summary)?;
    out.flush()
}

fn human(envelope: &Value, color: bool) -> io::Result<()> {
    if !envelope["error"].is_null() {
        let mut stderr = io::stderr().lock();
        writeln!(
            stderr,
            "{}: {}",
            safe(envelope["error"]["code"].as_str().unwrap_or("error")),
            safe(envelope["error"]["message"].as_str().unwrap_or(""))
        )?;
    }
    let data = &envelope["data"];
    if !data.is_null() {
        let mut out = io::stdout().lock();
        if let Some(items) = data.get("items").and_then(Value::as_array) {
            if let Some(first) = items.first() {
                let header = if first.get("tag").is_some() {
                    "ТЕГ\tЗАМЕТОК"
                } else if first.get("source_id").is_some() {
                    "ИСТОЧНИК\tЦЕЛЬ\tРАЗРЕШЕНА\tПРИЧИНА"
                } else {
                    "ID\tСТАДИЯ\tЗАГОЛОВОК\tПУТЬ"
                };
                if color {
                    write!(out, "\x1b[1m")?;
                }
                writeln!(out, "{header}")?;
                if color {
                    write!(out, "\x1b[0m")?;
                }
                for item in items {
                    if first.get("tag").is_some() {
                        writeln!(out, "{}\t{}", field(item, "tag"), item["count"])?;
                    } else if first.get("source_id").is_some() {
                        writeln!(
                            out,
                            "{}\t{}\t{}\t{}",
                            field(item, "source_id"),
                            field(item, "target"),
                            field(item, "resolved_id"),
                            field(item, "reason")
                        )?;
                    } else {
                        writeln!(
                            out,
                            "{}\t{}\t{}\t{}",
                            field(item, "id"),
                            field(item, "stage"),
                            field(item, "title"),
                            field(item, "path")
                        )?;
                    }
                }
            }
        } else if data.get("id").is_some() && data.get("nodes").is_none() {
            writeln!(out, "{}  {}", field(data, "id"), field(data, "path"))?;
            if let Some(title) = data.get("title").and_then(Value::as_str) {
                writeln!(out, "{}", safe(title))?;
            }
            if let Some(body) = data.get("body").and_then(Value::as_str) {
                write!(out, "{}", safe(body))?;
            }
        } else {
            writeln!(
                out,
                "{}",
                safe(&serde_json::to_string_pretty(data).unwrap())
            )?;
        }
        out.flush()?;
    }
    human_notices(envelope)
}

fn human_notices(envelope: &Value) -> io::Result<()> {
    let mut stderr = io::stderr().lock();
    if envelope["freshness"]["mode"] == "cached" {
        writeln!(
            stderr,
            "cached · generation {} · без сверки файлов",
            envelope["freshness"]["generation"]
        )?;
    }
    for kind in ["warnings", "diagnostics"] {
        for item in envelope[kind].as_array().unwrap() {
            writeln!(
                stderr,
                "{}: {}",
                field(
                    item,
                    if kind == "warnings" {
                        "code"
                    } else {
                        "rule_id"
                    }
                ),
                field(item, "message")
            )?;
        }
    }
    for path in envelope["recovery"].as_array().unwrap() {
        writeln!(
            stderr,
            "Восстановление: {}",
            safe(path.as_str().unwrap_or(""))
        )?;
    }
    stderr.flush()
}
fn field(value: &Value, name: &str) -> String {
    safe(value[name].as_str().unwrap_or("—"))
}
