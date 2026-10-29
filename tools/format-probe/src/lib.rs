//! Проверочный стенд этапа 0; не API и не реализация приложения.

pub use zetto::format::{parse_yaml as parse, yaml_diagnostics as diagnostics};

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use serde_json::Value;

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
