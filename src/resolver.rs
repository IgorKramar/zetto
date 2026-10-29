use crate::app::Record;
use crate::format::valid_id;
use crate::note::{Diagnostic, Link, LinkKind, Severity};
use crate::vault::Vault;
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

fn choose(records: &[Record], matches: Vec<usize>, link: &Link) -> Resolution {
    if matches.len() > 1 {
        return outcome(ResolutionKind::Ambiguous, link);
    }
    let Some(index) = matches.first().copied() else {
        return outcome(ResolutionKind::Broken, link);
    };
    let candidate = records[index]
        .note
        .id()
        .or_else(|| crate::app::filename_id(&records[index].path));
    if candidate.is_some_and(|id| records.iter().filter(|r| r.claims_id(id)).count() > 1) {
        return outcome(ResolutionKind::Ambiguous, link);
    }
    if records[index].id().is_none() {
        return outcome(ResolutionKind::Invalid, link);
    }
    Resolution {
        kind: ResolutionKind::Resolved,
        target: Some(index),
        diagnostic: None,
    }
}

fn by_id(records: &[Record], id: &str, link: &Link) -> Resolution {
    choose(
        records,
        records
            .iter()
            .enumerate()
            .filter(|(_, r)| r.claims_id(id))
            .map(|(i, _)| i)
            .collect(),
        link,
    )
}

pub fn resolve(vault: &Vault, records: &[Record], source: &str, link: &Link) -> Resolution {
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
                if link.kind == LinkKind::Wiki {
                    if external {
                        ResolutionKind::ExternalWiki
                    } else {
                        ResolutionKind::Unsafe
                    }
                } else if external {
                    ResolutionKind::External
                } else {
                    ResolutionKind::Unsafe
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
                by_id(records, raw, link)
            } else {
                outcome(ResolutionKind::Invalid, link)
            };
        }
        if raw.contains(['/', '\\']) && relative_path(raw).is_none() {
            return outcome(ResolutionKind::Unsafe, link);
        }
        let exact: Vec<_> = records
            .iter()
            .enumerate()
            .filter(|(_, r)| r.path == raw || r.path.strip_suffix(".md") == Some(raw))
            .map(|(i, _)| i)
            .collect();
        if !exact.is_empty() {
            return choose(records, exact, link);
        }
        let alias = raw.trim().to_lowercase();
        let aliases: Vec<_> = records
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                r.note
                    .aliases()
                    .iter()
                    .any(|a| a.trim().to_lowercase() == alias)
            })
            .map(|(i, _)| i)
            .collect();
        if !aliases.is_empty() {
            return choose(records, aliases, link);
        }
        let stem = raw.strip_suffix(".md").unwrap_or(raw);
        return choose(
            records,
            records
                .iter()
                .enumerate()
                .filter(|(_, r)| {
                    Path::new(&r.path).file_stem().and_then(|p| p.to_str()) == Some(stem)
                })
                .map(|(i, _)| i)
                .collect(),
            link,
        );
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
    let exact: Vec<_> = records
        .iter()
        .enumerate()
        .filter(|(_, r)| r.path == target)
        .map(|(i, _)| i)
        .collect();
    if !exact.is_empty() {
        return choose(records, exact, link);
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
        let mut resolved = by_id(records, prefix, link);
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
    choose(
        records,
        records
            .iter()
            .enumerate()
            .filter(|(_, r)| Path::new(&r.path).file_stem().and_then(|p| p.to_str()) == Some(stem))
            .map(|(i, _)| i)
            .collect(),
        link,
    )
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
