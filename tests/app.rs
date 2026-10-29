use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use tempfile::tempdir;
use zetto::app::{App, Stage, StageFilter};
use zetto::note::{Link, LinkKind, Span};
use zetto::resolver;
use zetto::vault::RecoveryMode;

fn setup() -> (tempfile::TempDir, App, String) {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vault");
    let initialized = App::init(&path, "Корень").unwrap();
    let root = initialized.data["id"].as_str().unwrap().to_owned();
    (dir, App::open(&path).unwrap(), root)
}

fn id(value: &zetto::app::Outcome) -> &str {
    value.data["id"].as_str().unwrap()
}

#[test]
fn capture_link_publish_cycle_and_readonly() {
    let (_dir, app, root) = setup();
    let body = "Мысль\r\nбез потери bytes";
    let draft = app.capture("Заголовок", body).unwrap();
    let draft_id = id(&draft);
    assert!(draft.saved);
    assert!(draft.data["path"].as_str().unwrap().starts_with("inbox/"));
    assert_eq!(app.show(draft_id).unwrap().data["body"], body);
    assert_eq!(
        app.publish(draft_id, None).unwrap_err().code,
        "publish_rejected"
    );
    app.link(draft_id, &root, Some("Контекст")).unwrap();
    assert!(!app.link(draft_id, &root, None).unwrap().saved);
    let published = app.publish(draft_id, None).unwrap();
    assert!(!published.data["path"].as_str().unwrap().contains('/'));
    let before = app.raw(draft_id).unwrap();
    app.scan().unwrap();
    app.show(draft_id).unwrap();
    app.list(StageFilter::All, None, 100, 0).unwrap();
    assert_eq!(app.raw(draft_id).unwrap(), before);
    assert!(!app.publish(draft_id, None).unwrap().saved);
    assert!(
        !app.publish(draft_id, Some("01ARZ3NDEKTSV4RRFFQ69G5FAV"))
            .unwrap()
            .saved
    );
    assert!(
        app.show(&root)
            .unwrap()
            .diagnostics
            .iter()
            .any(|d| d.rule_id == "zetto/root-exception")
    );
}

#[test]
fn new_failure_retains_text_and_non_qualifying_links_do_not_publish() {
    let (_dir, app, root) = setup();
    let body = "Текст\n";
    let error = app.new_note("", body, None).unwrap_err();
    assert!(error.saved);
    let data = error.data.unwrap();
    assert_eq!(
        app.show(data["id"].as_str().unwrap()).unwrap().data["body"],
        body
    );
    let draft = app.capture("Другой draft", "черновик").unwrap();
    for link_body in [
        "[URL](https://example.org)".into(),
        format!("`[[{root}]]`"),
        format!("[[{}]]", id(&draft)),
        format!("[[{root}#Heading]]"),
        "[[missing]]".into(),
    ] {
        let note = app.capture("Тема", &link_body).unwrap();
        assert_eq!(
            app.publish(id(&note), None).unwrap_err().code,
            "publish_rejected"
        );
        assert_eq!(app.show(id(&note)).unwrap().data["body"], link_body);
    }
    let self_link = app.capture("Себя", "").unwrap();
    app.link(id(&self_link), id(&self_link), None).unwrap();
    assert_eq!(
        app.publish(id(&self_link), None).unwrap_err().code,
        "publish_rejected"
    );
}

#[test]
fn invalid_files_and_duplicate_identity_remain_visible() {
    let (_dir, app, root) = setup();
    fs::write(
        app.vault.root().join("bad.md"),
        "---\ntitle: [bad\n---\nТекст",
    )
    .unwrap();
    let original = app
        .scan()
        .unwrap()
        .records
        .into_iter()
        .find(|r| r.note.id() == Some(&root))
        .unwrap();
    fs::write(
        app.vault.root().join(format!("{root}-duplicate.md")),
        original.raw.unwrap(),
    )
    .unwrap();
    let listed = app.list(StageFilter::All, None, 100, 0).unwrap();
    let items = listed.data["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert!(items.iter().all(|r| r["id"].is_null()));
    assert_eq!(
        app.link(&root, &root, None).unwrap_err().code,
        "duplicate_id"
    );
}

#[test]
fn resolver_matches_all_stage_zero_examples() {
    let corpus: Value = serde_json::from_str(include_str!("corpus/links.json")).unwrap();
    for case in corpus["cases"].as_array().unwrap() {
        let vault_name = case["vault"].as_str().unwrap_or("small");
        let app = App::open(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/corpus")
                .join(vault_name),
        )
        .unwrap();
        let catalog = app.scan().unwrap();
        let kind = match case["kind"].as_str().unwrap() {
            "wiki" => LinkKind::Wiki,
            "embed" => LinkKind::Embed,
            _ => LinkKind::Markdown,
        };
        let raw = case["raw"].as_str().unwrap();
        let link = Link {
            target: raw.split('|').next().unwrap().into(),
            display: None,
            kind,
            span: Span {
                start: 0,
                end: raw.len(),
            },
        };
        let resolution = resolver::resolve(
            &app.vault,
            &catalog.records,
            case["source"].as_str().unwrap(),
            &link,
        );
        assert_eq!(
            resolution.kind.as_str(),
            case["outcome"].as_str().unwrap(),
            "{}",
            case["name"]
        );
        if let Some(target_id) = case["target_id"].as_str() {
            let target = &catalog.records[resolution.target.unwrap()];
            assert_eq!(target.note.id(), Some(target_id), "{}", case["name"]);
            if let Some(expected) = case["satisfies_publish"].as_bool() {
                assert_eq!(
                    target.stage == Stage::Published
                        && target.path != case["source"].as_str().unwrap(),
                    expected
                );
            }
        }
        if let Some(rule) = case["diagnostic"].as_str() {
            assert_eq!(resolution.diagnostic.as_ref().unwrap().rule_id, rule);
        }
    }
}

#[test]
fn unsupported_format_allows_raw_and_refuses_mutation() {
    let (_dir, app, root) = setup();
    let config = app.vault.root().join(".zettorc");
    let text = fs::read_to_string(&config)
        .unwrap()
        .replace("format_version = 1", "format_version = 2");
    fs::write(config, text).unwrap();
    let future = App::open(app.vault.root()).unwrap();
    assert!(!future.raw(&root).unwrap().is_empty());
    assert_eq!(
        future.capture("X", "text").unwrap_err().code,
        "unsupported_format"
    );
}

#[test]
fn doctor_validates_work_before_recovery() {
    let (_dir, app, root) = setup();
    let record = app
        .scan()
        .unwrap()
        .records
        .into_iter()
        .find(|r| r.id() == Some(root.as_str()))
        .unwrap();
    let plan = {
        let guard = app.vault.lock().unwrap();
        app.vault.begin_edit(&guard, &record.path).unwrap()
    };
    fs::write(app.vault.path(&plan.work).unwrap(), "invalid note").unwrap();
    let before = app.raw(&root).unwrap();
    assert_eq!(
        App::doctor_at(
            app.vault.root(),
            Some(&plan.operation_id),
            Some(RecoveryMode::Resume)
        )
        .unwrap_err()
        .code,
        "validation_failed"
    );
    assert_eq!(app.raw(&root).unwrap(), before);
    App::doctor_at(
        app.vault.root(),
        Some(&plan.operation_id),
        Some(RecoveryMode::Restore),
    )
    .unwrap();
}

#[test]
fn doctor_resumes_init_with_only_durable_intent() {
    use zetto::vault::{PlanKind, PlanState};
    let (_dir, app, root) = setup();
    let mut plan = app
        .vault
        .plans()
        .unwrap()
        .into_iter()
        .find(|p| p.kind == PlanKind::Init)
        .unwrap();
    plan.state = PlanState::Preparing;
    for path in [&plan.to, &plan.work, &plan.backup, ".zettorc"] {
        fs::remove_file(app.vault.path(path).unwrap()).unwrap();
    }
    fs::write(
        app.vault
            .path(&format!(".zetto/recovery/{}/plan.json", plan.operation_id))
            .unwrap(),
        serde_json::to_vec(&plan).unwrap(),
    )
    .unwrap();
    App::doctor_at(
        app.vault.root(),
        Some(&plan.operation_id),
        Some(RecoveryMode::Resume),
    )
    .unwrap();
    let restored = App::open(app.vault.root()).unwrap();
    assert_eq!(restored.config.root_id, root);
    assert_eq!(restored.scan().unwrap().records.len(), 1);
}

#[cfg(unix)]
#[test]
fn doctor_resume_of_interrupted_restore_finishes_undo() {
    use std::os::unix::fs::PermissionsExt;
    let (_dir, app, root) = setup();
    let draft = app.capture("Мысль", "Текст").unwrap();
    let output = app.publish(id(&draft), Some(&root)).unwrap();
    let operation = app
        .vault
        .plans()
        .unwrap()
        .into_iter()
        .find(|p| p.kind == zetto::vault::PlanKind::Publish)
        .unwrap();
    fs::set_permissions(app.vault.root(), fs::Permissions::from_mode(0o500)).unwrap();
    let result = App::doctor_at(
        app.vault.root(),
        Some(&operation.operation_id),
        Some(RecoveryMode::Restore),
    );
    fs::set_permissions(app.vault.root(), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.is_err());
    App::doctor_at(
        app.vault.root(),
        Some(&operation.operation_id),
        Some(RecoveryMode::Resume),
    )
    .unwrap();
    assert!(
        app.vault
            .path(draft.data["path"].as_str().unwrap())
            .unwrap()
            .exists()
    );
    assert!(
        !app.vault
            .path(output.data["path"].as_str().unwrap())
            .unwrap()
            .exists()
    );
}

#[cfg(unix)]
#[test]
fn editor_success_failure_and_conflict_preserve_data() {
    let (dir, app, root) = setup();
    let draft = app.capture("Тема", "Before").unwrap();
    let draft_path = app.show(id(&draft)).unwrap().data["path"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut note = zetto::format::parse(&String::from_utf8(app.raw(id(&draft)).unwrap()).unwrap());
    note.fields.insert("updated".into(), json!(false));
    note.fields
        .insert("x-keep".into(), json!({"nested":[null,true,12]}));
    fs::write(
        app.vault.path(&draft_path).unwrap(),
        zetto::format::render(&note.fields, &note.body).unwrap(),
    )
    .unwrap();
    let script = dir.path().join("editor.py");
    fs::write(&script, "import pathlib,sys\np=pathlib.Path(sys.argv[-1])\nt=p.read_text().replace('Before','After')\nq=p.with_suffix('.new');q.write_text(t);q.replace(p)\n").unwrap();
    app.edit(
        id(&draft),
        &["python3".into(), script.to_string_lossy().into()],
    )
    .unwrap();
    assert_eq!(app.show(id(&draft)).unwrap().data["body"], "After");
    assert_eq!(
        app.show(id(&draft)).unwrap().data["metadata"]["updated"],
        false
    );
    assert_eq!(
        app.show(id(&draft)).unwrap().data["metadata"]["x-keep"],
        json!({"nested":[null,true,12]})
    );
    fs::write(&script, "import pathlib,sys\np=pathlib.Path(sys.argv[-1]);p.write_text(p.read_text()+'Unsaved?');sys.exit(9)\n").unwrap();
    let error = app
        .edit(
            id(&draft),
            &["python3".into(), script.to_string_lossy().into()],
        )
        .unwrap_err();
    assert_eq!(error.code, "editor_failed");
    assert!(error.recovery.iter().any(|p| {
        app.vault
            .read(p)
            .is_ok_and(|b| String::from_utf8_lossy(&b).contains("Unsaved?"))
    }));
    let path = app.show(&root).unwrap().data["path"]
        .as_str()
        .unwrap()
        .to_owned();
    let original = app.vault.path(&path).unwrap();
    fs::write(&script, format!("import pathlib,sys\np=pathlib.Path(sys.argv[-1]);p.write_text(p.read_text()+'Editor version')\nq=pathlib.Path({});q.write_text('External version without frontmatter')\n", json!(original.to_string_lossy()))).unwrap();
    let error = app
        .edit(&root, &["python3".into(), script.to_string_lossy().into()])
        .unwrap_err();
    assert_eq!(error.code, "conflict");
    assert!(
        String::from_utf8(app.raw(&root).unwrap())
            .unwrap()
            .contains("External version")
    );
    assert!(error.recovery.iter().any(|p| {
        app.vault
            .read(p)
            .is_ok_and(|b| String::from_utf8_lossy(&b).contains("Editor version"))
    }));
}
