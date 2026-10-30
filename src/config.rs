use crate::error::{Error, Result};
use crate::format::valid_id;
use crate::vault::Vault;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VaultConfig {
    pub config_version: u32,
    pub format_version: u32,
    #[serde(default)]
    pub root_id: String,
}

impl VaultConfig {
    pub fn parse(text: &str) -> Result<Self> {
        toml::from_str(text)
            .map_err(|e| Error::new("invalid_config", format!("Неверная .zettorc: {e}")))
    }

    pub fn load(vault: &Vault) -> Result<Self> {
        let bytes = vault.read(".zettorc")?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| Error::new("invalid_config", ".zettorc должна быть UTF-8"))?;
        Self::parse(text)
    }

    pub fn supported(&self) -> Result<()> {
        if self.config_version != 1 || self.format_version != 1 {
            return Err(Error::new(
                "unsupported_format",
                "Версия vault не поддерживается; доступно show --raw",
            ));
        }
        if !valid_id(&self.root_id) {
            return Err(Error::new(
                "invalid_config",
                "Нужен корректный root_id в .zettorc",
            ));
        }
        Ok(())
    }
}

pub fn discover(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_owned());
    }
    if let Some(path) = std::env::var_os("ZETTO_VAULT") {
        return Ok(path.into());
    }
    for path in std::env::current_dir()?.ancestors() {
        if path.join(".zettorc").exists() {
            return Ok(path.into());
        }
    }
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".zetto/notes"))
        .ok_or_else(|| Error::new("invalid_config", "Укажите --vault или HOME"))
}
