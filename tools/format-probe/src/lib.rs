//! Проверочный стенд этапа 0; не API и не реализация приложения.

use serde_json::Value;

pub fn parse(yaml: &str) -> Result<Value, serde_saphyr::Error> {
    serde_saphyr::from_str_with_options(
        yaml,
        serde_saphyr::options! {
            strict_booleans: true,
            reject_unsupported_tags: true,
            // Иначе YAML-ключи 1 и '1' молча схлопываются в String.
            no_schema: true,
        },
    )
}

/// Проверки независимы: ошибка optional-поля не скрывает обязательные поля.
pub fn diagnostics(value: &Value) -> Vec<String> {
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
    if let Some(id) = fields.get("id").and_then(Value::as_str)
        && (id.len() != 26
            || id.as_bytes()[0] > b'7'
            || !id
                .bytes()
                .all(|b| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&b))
            || id.parse::<ulid::Ulid>().is_err())
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Case {
        name: String,
        yaml: String,
        #[serde(default)]
        rejected: bool,
        #[serde(default)]
        expected: Option<Value>,
        #[serde(default)]
        diagnostics: Vec<String>,
    }

    #[test]
    fn yaml_corpus() {
        let cases: Vec<Case> =
            serde_json::from_str(include_str!("../../../tests/corpus/yaml.json")).unwrap();
        for case in cases {
            let parsed = parse(&case.yaml);
            if case.rejected {
                assert!(
                    parsed.is_err(),
                    "{}: должен быть отказ, получено {parsed:?}",
                    case.name
                );
                continue;
            }
            let value = parsed.unwrap_or_else(|e| panic!("{}: {e}", case.name));
            if let Some(expected) = case.expected {
                assert_eq!(value, expected, "{}: значения", case.name);
            }
            let mut expected = case.diagnostics;
            expected.sort();
            assert_eq!(diagnostics(&value), expected, "{}: диагностики", case.name);
            let emitted = serde_saphyr::to_string(&value).unwrap();
            let reparsed = parse(&emitted).unwrap();
            assert_eq!(reparsed, value, "{}: round-trip значений", case.name);
            assert_eq!(
                serde_saphyr::to_string(&reparsed).unwrap(),
                emitted,
                "{}: стабильность канонической записи",
                case.name
            );
        }
    }
}
