use crate::error::{Error, Result};
#[cfg(unix)]
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn command() -> Result<Vec<String>> {
    let local = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
        .map(|p| p.join("zetto/config.toml"));
    if let Some(path) = local {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let config: toml::Value = toml::from_str(&text).map_err(|e| {
                    Error::new(
                        "invalid_config",
                        format!("Неверные локальные настройки: {e}"),
                    )
                })?;
                if let Some(editor) = config.get("editor") {
                    let args = editor
                        .as_array()
                        .and_then(|a| {
                            a.iter()
                                .map(|v| v.as_str().map(str::to_owned))
                                .collect::<Option<Vec<_>>>()
                        })
                        .ok_or_else(|| {
                            Error::new("invalid_config", "editor должен быть массивом аргументов")
                        })?;
                    return checked(args);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    for key in ["VISUAL", "EDITOR"] {
        if let Some(value) = std::env::var_os(key) {
            let text = value
                .into_string()
                .map_err(|_| Error::new("invalid_config", format!("{key} должен быть UTF-8")))?;
            return checked(shell_words::split(&text).map_err(|e| {
                Error::new("invalid_config", format!("Неверная команда {key}: {e}"))
            })?);
        }
    }
    Err(Error::new(
        "invalid_config",
        "Задайте editor в локальном config.toml, VISUAL или EDITOR",
    ))
}

fn checked(args: Vec<String>) -> Result<Vec<String>> {
    if args.first().is_none_or(|s| s.trim().is_empty()) {
        Err(Error::new("invalid_config", "Команда редактора пуста"))
    } else {
        Ok(args)
    }
}

pub fn run(args: &[String], work: &Path, cwd: &Path) -> Result<()> {
    let program = args
        .first()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| Error::new("invalid_config", "Команда редактора пуста"))?;
    let mut command = Command::new(program);
    #[cfg(unix)]
    command.stdout(std::io::stderr().as_fd().try_clone_to_owned()?);
    let status = command
        .args(&args[1..])
        .arg(work)
        .current_dir(cwd)
        .status()
        .map_err(|e| {
            Error::new(
                "editor_failed",
                format!("Не удалось запустить редактор: {e}"),
            )
        })?;
    if !status.success() {
        return Err(Error::new(
            "editor_failed",
            format!("Редактор завершился с {status}"),
        ));
    }
    Ok(())
}
