use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    #[serde(rename = "warn")]
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Diagnostic {
    pub rule_id: String,
    pub severity: Severity,
    pub path: Option<String>,
    pub span: Option<Span>,
    pub message: String,
    pub fix: Option<Value>,
}

impl Diagnostic {
    pub fn new(rule_id: &str, severity: Severity, message: impl Into<String>) -> Self {
        Self {
            rule_id: rule_id.into(),
            severity,
            path: None,
            span: None,
            message: message.into(),
            fix: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    Wiki,
    Markdown,
    Embed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Link {
    pub target: String,
    pub display: Option<String>,
    pub kind: LinkKind,
    pub span: Span,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Note {
    pub fields: Map<String, Value>,
    pub body: String,
    pub links: Vec<Link>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Note {
    pub fn id(&self) -> Option<&str> {
        self.fields.get("id").and_then(Value::as_str)
    }

    pub fn title(&self) -> Option<&str> {
        self.fields.get("title").and_then(Value::as_str)
    }

    pub fn aliases(&self) -> Vec<String> {
        string_list(self.fields.get("aliases"), false)
    }

    pub fn tags(&self) -> Vec<String> {
        string_list(self.fields.get("tags"), true)
    }

    pub fn writable(&self) -> bool {
        self.diagnostics
            .iter()
            .all(|d| d.severity != Severity::Error)
    }
}

fn string_list(value: Option<&Value>, scalar: bool) -> Vec<String> {
    match value {
        Some(Value::String(s)) if scalar => vec![s.clone()],
        Some(Value::Array(values)) if values.iter().all(Value::is_string) => values
            .iter()
            .filter_map(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}
