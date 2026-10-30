use crate::app::Record;
use crate::format::valid_id;
use crate::note::{Diagnostic, Link, LinkKind, Severity};
use crate::vault::Vault;
use std::collections::HashMap;
use std::path::{Component, Path};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolutionKind {
    Resolved,
    Broken,
    Ambiguous,
    Invalid,
    Unsupported,
    Unsafe,
    External,
    ExternalWiki,
    Attachment,
}
impl ResolutionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Resolved => "resolved",
            Self::Broken => "broken",
            Self::Ambiguous => "ambiguous",
            Self::Invalid => "invalid",
            Self::Unsupported => "unsupported",
            Self::Unsafe => "unsafe",
            Self::External => "external",
            Self::ExternalWiki => "external-as-wiki",
            Self::Attachment => "attachment",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Resolution {
    pub kind: ResolutionKind,
    pub target: Option<usize>,
    pub diagnostic: Option<Diagnostic>,
}

fn outcome(kind: ResolutionKind, link: &Link) -> Resolution {
    let rule = match kind {
        ResolutionKind::Broken => Some("zetto/no-broken-link"),
        ResolutionKind::Ambiguous => Some("zetto/ambiguous-link"),
        ResolutionKind::Invalid => Some("zetto/non-ulid-wikilink-target"),
        ResolutionKind::Unsafe => Some("zetto/unsafe-link"),
        ResolutionKind::ExternalWiki => Some("zetto/external-url-as-wikilink"),
        ResolutionKind::Unsupported if link.kind == LinkKind::Embed => {
            Some("zetto/embed-not-supported-in-v1")
        }
        ResolutionKind::Unsupported if link.target.contains("#^") => {
            Some("zetto/block-ref-not-supported-in-v1")
        }
        ResolutionKind::Unsupported => Some("zetto/anchor-not-supported-in-v1"),
        _ => None,
    };
    let diagnostic = rule.map(|rule| {
        let mut d = Diagnostic::new(
            rule,
            Severity::Warning,
            format!("Ссылка {}: {}", link.target, kind.as_str()),
        );
        d.span = Some(link.span);
        d
    });
    Resolution {
        kind,
        target: None,
        diagnostic,
    }
}

/// Карты принадлежат одному снимку; порядок категорий совпадает с format-v1.
pub struct Resolver<'a> {
    records: &'a [Record],
    claims: HashMap<String, Vec<usize>>,
    wiki_paths: HashMap<String, Vec<usize>>,
    paths: HashMap<String, Vec<usize>>,
    aliases: HashMap<String, Vec<usize>>,
    stems: HashMap<String, Vec<usize>>,
}

impl<'a> Resolver<'a> {
    pub fn new(records: &'a [Record]) -> Self {
        let mut resolver = Self {
            records,
            claims: HashMap::new(),
            wiki_paths: HashMap::new(),
            paths: HashMap::new(),
            aliases: HashMap::new(),
            stems: HashMap::new(),
        };
        for (index, record) in records.iter().enumerate() {
            for id in [record.note.id(), crate::app::filename_id(&record.path)]
                .into_iter()
                .flatten()
            {
                add(&mut resolver.claims, id, index);
            }
            add(&mut resolver.paths, &record.path, index);
            add(&mut resolver.wiki_paths, &record.path, index);
            if let Some(path) = record.path.strip_suffix(".md") {
                add(&mut resolver.wiki_paths, path, index);
            }
            for alias in record.note.aliases() {
                add(&mut resolver.aliases, &alias.trim().to_lowercase(), index);
            }
            if let Some(stem) = Path::new(&record.path).file_stem().and_then(|p| p.to_str()) {
                add(&mut resolver.stems, stem, index);
            }
        }
        resolver
    }

    fn choose(&self, matches: Option<&Vec<usize>>, link: &Link) -> Resolution {
        let matches = matches.map(Vec::as_slice).unwrap_or_default();
        if matches.len() > 1 {
            return outcome(ResolutionKind::Ambiguous, link);
        }
        let Some(index) = matches.first().copied() else {
            return outcome(ResolutionKind::Broken, link);
        };
        let record = &self.records[index];
        let candidate = record
            .note
            .id()
            .or_else(|| crate::app::filename_id(&record.path));
        if candidate
            .and_then(|id| self.claims.get(id))
            .is_some_and(|v| v.len() > 1)
        {
            return outcome(ResolutionKind::Ambiguous, link);
        }
        if record.id().is_none() {
            return outcome(ResolutionKind::Invalid, link);
        }
        Resolution {
            kind: ResolutionKind::Resolved,
            target: Some(index),
            diagnostic: None,
        }
    }

    pub fn resolve(&self, vault: &Vault, source: &str, link: &Link) -> Resolution {
        let raw = link.target.as_str();
        if raw.is_empty() {
            return outcome(ResolutionKind::Broken, link);
        }
        if link.kind == LinkKind::Embed {
            return outcome(ResolutionKind::Unsupported, link);
        }
        if let Some((scheme, _)) = raw.split_once(':').filter(|(scheme, _)| uri_scheme(scheme)) {
            let external = ["http", "https", "ftp", "sftp", "ssh", "mailto"]
                .contains(&scheme.to_ascii_lowercase().as_str());
            let dangerous =
                ["file", "javascript", "data"].contains(&scheme.to_ascii_lowercase().as_str());
            if external || dangerous || !raw.chars().any(char::is_whitespace) {
                return outcome(
                    if !external {
                        ResolutionKind::Unsafe
                    } else if link.kind == LinkKind::Wiki {
                        ResolutionKind::ExternalWiki
                    } else {
                        ResolutionKind::External
                    },
                    link,
                );
            }
        }
        if raw.contains('#') {
            return outcome(ResolutionKind::Unsupported, link);
        }
        if link.kind == LinkKind::Wiki {
            if raw.chars().count() == 26 && !raw.contains(['/', '\\', '.']) {
                return if valid_id(raw) {
                    self.choose(self.claims.get(raw), link)
                } else {
                    outcome(ResolutionKind::Invalid, link)
                };
            }
            if raw.contains(['/', '\\']) && relative_path(raw).is_none() {
                return outcome(ResolutionKind::Unsafe, link);
            }
            if let Some(matches) = self.wiki_paths.get(raw) {
                return self.choose(Some(matches), link);
            }
            let alias = raw.trim().to_lowercase();
            if let Some(matches) = self.aliases.get(&alias) {
                return self.choose(Some(matches), link);
            }
            let stem = raw.strip_suffix(".md").unwrap_or(raw);
            return self.choose(self.stems.get(stem), link);
        }
        let Some(decoded) = percent_decode(raw) else {
            return outcome(ResolutionKind::Unsafe, link);
        };
        let Some(relative) = relative_path(&decoded) else {
            return outcome(ResolutionKind::Unsafe, link);
        };
        let parent = Path::new(source).parent().unwrap_or_else(|| Path::new(""));
        let joined = parent.join(relative);
        let Some(target) = joined.to_str() else {
            return outcome(ResolutionKind::Unsafe, link);
        };
        let Ok(path) = vault.path(target) else {
            return outcome(ResolutionKind::Unsafe, link);
        };
        if let Some(matches) = self.paths.get(target) {
            return self.choose(Some(matches), link);
        }
        if target.starts_with("assets/") {
            return outcome(
                if path.is_file() {
                    ResolutionKind::Attachment
                } else {
                    ResolutionKind::Broken
                },
                link,
            );
        }
        let stem = Path::new(&decoded)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        if let Some(prefix) = stem.get(..26).filter(|p| valid_id(p)) {
            let mut resolved = self.choose(self.claims.get(prefix), link);
            if resolved.kind == ResolutionKind::Resolved {
                let mut d = Diagnostic::new(
                    "zetto/stale-link-path",
                    Severity::Warning,
                    "Имя цели устарело; ссылка разрешена по ID",
                );
                d.span = Some(link.span);
                resolved.diagnostic = Some(d);
            }
            return resolved;
        }
        self.choose(self.stems.get(stem), link)
    }
}

fn add(map: &mut HashMap<String, Vec<usize>>, key: &str, index: usize) {
    let values = map.entry(key.to_owned()).or_default();
    if values.last() != Some(&index) {
        values.push(index);
    }
}

pub fn resolve(vault: &Vault, records: &[Record], source: &str, link: &Link) -> Resolution {
    Resolver::new(records).resolve(vault, source, link)
}

fn relative_path(raw: &str) -> Option<String> {
    if raw.contains('\\') || raw.chars().any(char::is_control) {
        return None;
    }
    let mut parts = Vec::new();
    for part in Path::new(raw).components() {
        match part {
            Component::Normal(v) => parts.push(v.to_str()?),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

fn uri_scheme(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphabetic())
        && bytes.all(|b| b.is_ascii_alphanumeric() || b"+.-".contains(&b))
}

fn percent_decode(raw: &str) -> Option<String> {
    let mut out = Vec::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let a = (bytes.get(i + 1).copied()? as char).to_digit(16)?;
            let b = (bytes.get(i + 2).copied()? as char).to_digit(16)?;
            out.push((a * 16 + b) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}
