use serde::Deserialize;
use serde_json::{Value, json};
use zetto::format::{self, filename, parse, parse_yaml, render, valid_id};
use zetto::note::{LinkKind, Severity};

const ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

#[derive(Deserialize)]
struct YamlCase {
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
fn existing_yaml_corpus_uses_production_parser() {
    let cases: Vec<YamlCase> = serde_json::from_str(include_str!("corpus/yaml.json")).unwrap();
    for case in cases {
        let result = parse_yaml(&case.yaml);
        if case.rejected {
            assert!(result.is_err(), "{}: {result:?}", case.name);
            continue;
        }
        let value = result.unwrap_or_else(|error| panic!("{}: {error}", case.name));
        if let Some(expected) = case.expected {
            assert_eq!(value, expected, "{}", case.name);
        }
        let mut expected = case.diagnostics;
        expected.sort();
        assert_eq!(format::yaml_diagnostics(&value), expected, "{}", case.name);
        let emitted = serde_saphyr::to_string(&value).unwrap();
        assert_eq!(parse_yaml(&emitted).unwrap(), value, "{}", case.name);
    }
}

#[test]
fn markdown_ranges_match_original_utf8_files() {
    let cases: Value = serde_json::from_str(include_str!("corpus/markdown.json")).unwrap();
    for case in cases.as_array().unwrap() {
        let text = case["text"].as_str().unwrap();
        let note = parse(text);
        assert!(note.writable(), "{:?}", note.diagnostics);
        let expected = case["links"].as_array().unwrap();
        assert_eq!(note.links.len(), expected.len(), "{}", case["name"]);
        for (link, expected) in note.links.iter().zip(expected) {
            assert_eq!(
                link.span.start,
                expected["start"].as_u64().unwrap() as usize
            );
            assert_eq!(link.span.end, expected["end"].as_u64().unwrap() as usize);
            assert_eq!(
                &text[link.span.start..link.span.end],
                expected["raw"].as_str().unwrap()
            );
        }
    }
}

#[test]
fn optional_errors_and_unknown_values_survive_metadata_write() {
    let body = "\r\n# Мысль\r\n\r\nТекст без последнего перевода строки";
    let input = format!(
        "---\r\nid: {ID}\r\ntitle: Before\r\ntags: [rust, 12]\r\naliases: [A, 9]\r\nx-extra: {{nested: [null, true, 12]}}\r\n...\r\n{body}"
    );
    let mut note = parse(&input);
    assert!(note.writable());
    assert_eq!(note.id(), Some(ID));
    assert_eq!(note.body, body);
    assert!(note.tags().is_empty());
    assert!(note.aliases().is_empty());
    assert!(
        note.diagnostics
            .iter()
            .filter(|d| d.rule_id.contains("invalid-"))
            .all(|d| d.severity == Severity::Warning)
    );
    note.fields.insert("title".into(), json!("После"));
    let output = render(&note.fields, &note.body).unwrap();
    let reparsed = parse(&output);
    assert_eq!(reparsed.fields, note.fields);
    assert_eq!(reparsed.body.as_bytes(), body.as_bytes());
    assert_eq!(
        reparsed.fields["x-extra"],
        json!({"nested": [null, true, 12]})
    );
    assert_eq!(render(&reparsed.fields, &reparsed.body).unwrap(), output);
}

#[test]
fn malformed_required_fields_and_merge_conflicts_block_write() {
    for input in [
        "plain text",
        "---\ntitle: X\n",
        "\u{feff}---\ntitle: X\n---\n",
        "---\nid: broken\ntitle: X\n---\n",
        "---\ntitle: A\ntitle: B\n---\n",
    ] {
        assert!(!parse(input).writable(), "{input}");
    }
    let draft = parse(&format!("---\nid: {ID}\ntitle: ''\n---\nText"));
    assert!(draft.writable());
    assert!(
        draft
            .diagnostics
            .iter()
            .any(|d| d.rule_id == "zetto/empty-title")
    );
    let conflict = parse(&format!(
        "---\nid: {ID}\ntitle: X\n---\n<<<<<<< HEAD\nA\n=======\nB\n>>>>>>> branch\n"
    ));
    assert!(!conflict.writable());
    assert!(
        conflict
            .diagnostics
            .iter()
            .any(|d| d.rule_id == "zetto/merge-conflict")
    );
    assert!(parse(&format!("---\nid: {ID}\ntitle: X\n---\nHeading\n=======\n")).writable());
}

#[test]
fn ids_and_filenames_have_fixed_boundaries() {
    assert!(valid_id(ID));
    for id in [
        "",
        "ZZZZZZZZZZZZZZZZZZZZZZZZZZ",
        "01arz3ndektsv4rrffq69g5fav",
        "01ARZ3NDEKTSV4RRFFQ69G5FAI",
    ] {
        assert!(!valid_id(id));
    }
    assert_eq!(filename(ID, ""), format!("{ID}.md"));
    assert_eq!(
        filename(ID, "Связи и контекст"),
        format!("{ID}-sviazi-i-kontekst.md")
    );
    let long = filename(ID, &"a".repeat(90));
    assert!(long.is_ascii());
    assert_eq!(long.len(), 90);
    assert_eq!(
        filename(ID, &format!("{} words", "a".repeat(55))),
        format!("{ID}-{}.md", "a".repeat(55))
    );
}

#[test]
fn links_keep_display_and_unsupported_forms_outside_code() {
    let body = format!(
        "[[{ID}|a|b]] ![[{ID}]] [[{ID}#Heading]] [link](other.md)\n\n`[[ignored]]`\n\n    [[ignored]]\n"
    );
    let text = format!("---\nid: {ID}\ntitle: X\n---\n{body}");
    let note = parse(&text);
    assert_eq!(note.links.len(), 4, "{:?}", note.links);
    assert_eq!(note.links[0].kind, LinkKind::Wiki);
    assert_eq!(note.links[0].display.as_deref(), Some("a|b"));
    assert_eq!(note.links[1].kind, LinkKind::Embed);
    assert!(note.links[2].target.contains('#'));
    assert_eq!(note.links[3].kind, LinkKind::Markdown);
}

#[test]
fn resource_limits_are_visible_errors() {
    let input = format!(
        "---\nid: {ID}\ntitle: X\nx-deep: {}0{}\n---\n",
        "[".repeat(80),
        "]".repeat(80)
    );
    let note = parse(&input);
    assert!(!note.writable());
    assert!(
        note.diagnostics
            .iter()
            .any(|d| d.rule_id == "zetto/resource-limit"),
        "{:?}",
        note.diagnostics
    );
    let oversized = "x".repeat(format::MAX_NOTE_BYTES + 1);
    assert!(
        parse(&oversized)
            .diagnostics
            .iter()
            .any(|d| d.rule_id == "zetto/resource-limit")
    );
}
