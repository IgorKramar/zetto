use crate::note::{Diagnostic, Link, LinkKind, Note, Severity, Span};
use pulldown_cmark::{Event, LinkType, Options, Parser, Tag, TagEnd};
use serde_json::{Map, Value};

pub const MAX_NOTE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_FRONTMATTER_BYTES: usize = 1024 * 1024;
pub const MAX_YAML_DEPTH: usize = 64;

pub fn valid_id(id: &str) -> bool {
    id.len() == 26
        && id.as_bytes()[0] <= b'7'
        && id
            .bytes()
            .all(|b| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&b))
        && id.parse::<ulid::Ulid>().is_ok()
}

pub fn filename(id: &str, title: &str) -> String {
    let mut slug = slug::slugify(title);
    if slug.len() > 60 {
        let end = slug[..60].rfind('-').filter(|end| *end > 0).unwrap_or(60);
        slug.truncate(end);
    }
    if slug.is_empty() {
        format!("{id}.md")
    } else {
        format!("{id}-{slug}.md")
    }
}

pub fn parse_yaml(yaml: &str) -> Result<Value, serde_saphyr::Error> {
    if yaml.len() > MAX_FRONTMATTER_BYTES {
        return Err(<serde_saphyr::Error as serde::de::Error>::custom(
            "zetto/resource-limit: frontmatter превышает 1 MiB",
        ));
    }
    serde_saphyr::from_str_with_options(
        yaml,
        serde_saphyr::options! {
            strict_booleans: true,
            reject_unsupported_tags: true,
            // Не позволять YAML-ключам 1 и '1' схлопнуться в String.
            no_schema: true,
            budget: serde_saphyr::budget! {
                max_depth: MAX_YAML_DEPTH,
                flow_nesting_limit: MAX_YAML_DEPTH,
                max_events: 100_000,
                max_nodes: 50_000,
                max_aliases: 1_000,
                max_anchors: 1_000,
                max_total_scalar_bytes: MAX_FRONTMATTER_BYTES,
            },
        },
    )
}

/// Проверяет поля независимо; суффикс после ':' обозначает поле.
pub fn yaml_diagnostics(value: &Value) -> Vec<String> {
    let mut result = Vec::new();
    let Some(fields) = value.as_object() else {
        return vec!["zetto/frontmatter-not-mapping".into()];
    };
    for key in ["id", "title"] {
        if !fields.contains_key(key) {
            result.push(format!("zetto/missing-required-field:{key}"));
        } else if !fields[key].is_string() {
            result.push(format!("zetto/invalid-required-type:{key}"));
        }
    }
    if fields
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(|id| !valid_id(id))
    {
        result.push("zetto/invalid-id-format".into());
    }
    if fields
        .get("title")
        .and_then(Value::as_str)
        .is_some_and(|s| s.trim().is_empty())
    {
        result.push("zetto/empty-title".into());
    }
    for key in ["tags", "aliases"] {
        if let Some(field) = fields.get(key) {
            let list_ok = field
                .as_array()
                .is_some_and(|a| a.iter().all(Value::is_string));
            if key == "tags" && field.is_string() {
                result.push("zetto/non-canonical-tag-format".into());
            } else if !list_ok {
                result.push(format!("zetto/invalid-{key}-format"));
            }
        }
    }
    for key in ["created", "updated"] {
        if let Some(field) = fields.get(key)
            && !field
                .as_str()
                .is_some_and(|s| chrono::DateTime::parse_from_rfc3339(s).is_ok())
        {
            result.push(format!("zetto/non-rfc3339-timestamp:{key}"));
        }
    }
    result.sort();
    result
}

pub fn parse(input: &str) -> Note {
    let mut note = Note::default();
    if input.len() > MAX_NOTE_BYTES {
        note.diagnostics
            .push(error("zetto/resource-limit", "Заметка превышает 16 MiB"));
        return note;
    }
    let Some((yaml, body, body_offset)) = split_frontmatter(input) else {
        note.body = input.into();
        note.diagnostics.push(error(
            "zetto/invalid-frontmatter",
            "Нужен frontmatter с разделителями в начале файла",
        ));
        return note;
    };
    note.body = body.into();
    match parse_yaml(yaml) {
        Ok(value) => {
            note.diagnostics = yaml_diagnostics(&value)
                .into_iter()
                .map(|code| {
                    let (rule, field) = code.split_once(':').unwrap_or((&code, ""));
                    let severity = match rule {
                        "zetto/missing-required-field"
                        | "zetto/invalid-required-type"
                        | "zetto/invalid-id-format"
                        | "zetto/frontmatter-not-mapping" => Severity::Error,
                        "zetto/non-canonical-tag-format" => Severity::Info,
                        _ => Severity::Warning,
                    };
                    let message = if field.is_empty() {
                        rule.into()
                    } else {
                        format!("{rule}: поле {field}")
                    };
                    Diagnostic::new(rule, severity, message)
                })
                .collect();
            if let Value::Object(fields) = value {
                note.fields = fields;
            }
        }
        Err(err) => {
            let message = err.to_string();
            let rule = match &err {
                serde_saphyr::Error::Budget { .. }
                | serde_saphyr::Error::AliasReplayLimitExceeded { .. }
                | serde_saphyr::Error::AliasExpansionLimitExceeded { .. }
                | serde_saphyr::Error::AliasReplayStackDepthExceeded { .. } => {
                    "zetto/resource-limit"
                }
                _ if message.contains("recursion limit exceeded") => "zetto/resource-limit",
                serde_saphyr::Error::DuplicateMappingKey { .. } => {
                    "zetto/duplicate-frontmatter-key"
                }
                _ if yaml.len() > MAX_FRONTMATTER_BYTES => "zetto/resource-limit",
                _ => "zetto/invalid-yaml",
            };
            note.diagnostics.push(error(rule, message));
        }
    }
    for key in note.fields.keys() {
        if !key.starts_with("x-")
            && ![
                "id",
                "title",
                "tags",
                "aliases",
                "created",
                "updated",
                "slug_locked",
            ]
            .contains(&key.as_str())
        {
            note.diagnostics.push(Diagnostic::new(
                "zetto/unknown-frontmatter-field",
                Severity::Warning,
                format!("Неизвестное поле {key} сохранится при записи"),
            ));
        }
    }
    if note
        .fields
        .get("aliases")
        .and_then(Value::as_array)
        .is_some_and(|a| {
            a.iter()
                .any(|v| v.as_str().is_some_and(|s| s.trim().is_empty()))
        })
    {
        note.diagnostics.push(Diagnostic::new(
            "zetto/empty-alias",
            Severity::Warning,
            "Пустой alias не участвует в разрешении ссылок",
        ));
    }
    if input
        .lines()
        .any(|line| line.starts_with("<<<<<<< ") || line.starts_with(">>>>>>> "))
    {
        note.diagnostics.push(error(
            "zetto/merge-conflict",
            "Найдены маркеры неразрешённого git-конфликта",
        ));
    }
    note.links = extract_links(body, body_offset);
    for link in &note.links {
        let unsupported = if link.kind == LinkKind::Embed {
            Some((
                "zetto/embed-not-supported-in-v1",
                "Встраивание заметки не поддерживается",
            ))
        } else if link.target.contains("#^") {
            Some((
                "zetto/block-ref-not-supported-in-v1",
                "Ссылка на блок не поддерживается",
            ))
        } else if link.target.contains('#') {
            Some((
                "zetto/anchor-not-supported-in-v1",
                "Ссылка на заголовок не поддерживается",
            ))
        } else {
            None
        };
        if let Some((rule, message)) = unsupported {
            let mut diagnostic = Diagnostic::new(rule, Severity::Warning, message);
            diagnostic.span = Some(link.span);
            note.diagnostics.push(diagnostic);
        }
    }
    note
}

pub fn render(fields: &Map<String, Value>, body: &str) -> Result<String, String> {
    let yaml = serde_saphyr::to_string(fields).map_err(|error| error.to_string())?;
    if yaml.len() > MAX_FRONTMATTER_BYTES || yaml.len() + body.len() + 8 > MAX_NOTE_BYTES {
        return Err("zetto/resource-limit: заметка превышает лимит записи".into());
    }
    Ok(format!("---\n{yaml}---\n{body}"))
}

fn error(rule: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::new(rule, Severity::Error, message)
}

fn split_frontmatter(input: &str) -> Option<(&str, &str, usize)> {
    let opening = if input.starts_with("---\r\n") {
        5
    } else if input.starts_with("---\n") {
        4
    } else {
        return None;
    };
    let mut offset = opening;
    for line in input[opening..].split_inclusive('\n') {
        let text = line
            .strip_suffix("\r\n")
            .or_else(|| line.strip_suffix('\n'))
            .unwrap_or(line);
        if text == "---" || text == "..." {
            let body_offset = offset + line.len();
            return Some((&input[opening..offset], &input[body_offset..], body_offset));
        }
        offset += line.len();
    }
    None
}

pub fn extract_links(body: &str, offset: usize) -> Vec<Link> {
    let options = Options::ENABLE_WIKILINKS
        | Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS;
    let mut links = Vec::<Link>::new();
    let mut open = Vec::<(usize, TagEnd)>::new();
    for (event, range) in Parser::new_ext(body, options).into_offset_iter() {
        match event {
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                ..
            })
            | Event::Start(Tag::Image {
                link_type,
                dest_url,
                ..
            }) => {
                let image = body[range.clone()].starts_with('!');
                let kind = match link_type {
                    LinkType::WikiLink { .. } if image => LinkKind::Embed,
                    LinkType::WikiLink { .. } => LinkKind::Wiki,
                    _ => LinkKind::Markdown,
                };
                let display = if matches!(link_type, LinkType::WikiLink { has_pothole: true }) {
                    body[range.clone()]
                        .split_once('|')
                        .map(|(_, text)| text.trim_end_matches("]]").to_owned())
                } else if kind == LinkKind::Markdown {
                    Some(String::new())
                } else {
                    None
                };
                let end = if image { TagEnd::Image } else { TagEnd::Link };
                open.push((links.len(), end));
                links.push(Link {
                    target: dest_url.into_string(),
                    display,
                    kind,
                    span: Span {
                        start: offset + range.start,
                        end: offset + range.end,
                    },
                });
            }
            Event::Text(text) | Event::Code(text) => {
                if let Some((index, _)) = open.last()
                    && links[*index].kind == LinkKind::Markdown
                    && let Some(display) = &mut links[*index].display
                {
                    display.push_str(&text);
                }
            }
            Event::End(end @ (TagEnd::Link | TagEnd::Image)) => {
                if let Some((index, expected)) = open.last().copied()
                    && end == expected
                {
                    links[index].span.end = links[index].span.end.max(offset + range.end);
                    open.pop();
                }
            }
            _ => {}
        }
    }
    links
}
