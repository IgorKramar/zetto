use serde_json::Value;
use std::fs;
use zetto::app::{App, LinkDirection, SearchMode, StageFilter};
use zetto::index::{Cancellation, FreshnessMode, Index};

fn setup() -> (tempfile::TempDir, App, String) {
    let dir = tempfile::tempdir().unwrap();
    let out = App::init(&dir.path().join("vault"), "Корень").unwrap();
    let app = App::open(&dir.path().join("vault")).unwrap();
    (dir, app, out.data["id"].as_str().unwrap().to_owned())
}
fn identity(out: &zetto::app::Outcome) -> String {
    out.data["id"].as_str().unwrap().to_owned()
}
fn items(out: &zetto::app::Outcome) -> &[Value] {
    out.data["items"].as_array().unwrap()
}

#[test]
fn queries_share_index_and_distinguish_search_modes() {
    let (_dir, app, root) = setup();
    let a = app
        .capture("Кириллица", "alpha.*beta #мысль #мысль `#код`")
        .unwrap();
    let b = app.capture("Вторая", "alphaXYZbeta alpha").unwrap();
    let token = Cancellation::default();
    assert_eq!(
        items(
            &app.search("Кириллица", SearchMode::Default, 50, 0, &token)
                .unwrap()
        )
        .len(),
        1
    );
    assert_eq!(
        items(
            &app.search("alpha.*beta", SearchMode::Literal, 50, 0, &token)
                .unwrap()
        )
        .len(),
        1
    );
    assert_eq!(
        items(
            &app.search("alpha.*beta", SearchMode::Regex, 50, 0, &token)
                .unwrap()
        )
        .len(),
        2
    );
    assert_eq!(
        app.search("[", SearchMode::Regex, 50, 0, &token)
            .unwrap_err()
            .code,
        "usage"
    );
    assert_eq!(
        app.search("\"", SearchMode::Fts, 50, 0, &token)
            .unwrap_err()
            .code,
        "usage"
    );
    assert!(
        items(
            &app.search("несуществующее + \"", SearchMode::Default, 50, 0, &token)
                .unwrap()
        )
        .is_empty()
    );
    let page = app
        .search("alpha", SearchMode::Default, 1, 0, &token)
        .unwrap();
    assert!(!items(&page).is_empty());
    assert!(page.truncated && !page.complete);
    assert_eq!(page.data["next_offset"], 1);
    let next = app
        .search("alpha", SearchMode::Default, 1, 1, &token)
        .unwrap();
    assert_ne!(items(&page)[0]["id"], items(&next)[0]["id"]);
    assert!(!next.truncated);
    let tags = app.tags(&token).unwrap();
    assert_eq!(
        items(&tags),
        &[serde_json::json!({"tag":"мысль","count":1})]
    );
    assert_eq!(
        items(&app.list(StageFilter::All, Some("мысль"), 100, 0).unwrap()).len(),
        1
    );
    let picked = app
        .fuzzy_titles("крлц", StageFilter::All, 50, &token)
        .unwrap();
    assert_eq!(items(&picked)[0]["id"], identity(&a));
    let path = a.data["path"].as_str().unwrap();
    let raw = String::from_utf8(app.vault.read(path).unwrap()).unwrap();
    let mut note = zetto::format::parse(&raw);
    note.fields
        .insert("aliases".into(), serde_json::json!(["псевдоним"]));
    fs::write(
        app.vault.path(path).unwrap(),
        zetto::format::render(&note.fields, &note.body).unwrap(),
    )
    .unwrap();
    let aliases = app
        .fuzzy_titles("псевд", StageFilter::All, 50, &token)
        .unwrap();
    assert_eq!(items(&aliases)[0]["id"], identity(&a));
    let paths = app
        .paths(&[identity(&a), identity(&b), root], &token)
        .unwrap();
    assert_eq!(items(&paths).len(), 3);
    assert!(
        items(&paths)
            .iter()
            .all(|p| std::path::Path::new(p["path"].as_str().unwrap()).is_absolute())
    );
    assert_eq!(
        app.graph(&identity(&a), 0, &token).unwrap_err().code,
        "usage"
    );
}

#[test]
fn cached_links_are_one_persisted_generation() {
    let (_dir, app, root) = setup();
    let draft = app
        .capture("Источник", &format!("[[{root}]] [[missing]]"))
        .unwrap();
    let id = identity(&draft);
    let token = Cancellation::default();
    let links = app.links(&id, LinkDirection::Outgoing, &token).unwrap();
    assert_eq!(items(&links).len(), 2);
    assert_eq!(items(&links)[0]["resolved_id"], root);
    assert_eq!(items(&links)[1]["reason"], "broken");
    let graph = app.graph(&root, 1, &token).unwrap();
    assert_eq!(graph.data["nodes"].as_array().unwrap().len(), 2);
    let root_path = app.scan().unwrap().find(&root).unwrap().path.clone();
    fs::remove_file(app.vault.path(&root_path).unwrap()).unwrap();
    let cached = app
        .clone()
        .with_freshness(FreshnessMode::Cached)
        .links(&id, LinkDirection::Outgoing, &token)
        .unwrap();
    assert_eq!(cached.freshness.mode, FreshnessMode::Cached);
    assert_eq!(cached.freshness.generation, links.freshness.generation);
    assert_eq!(items(&cached)[0]["resolved_id"], root);
    let fresh = app.links(&id, LinkDirection::Outgoing, &token).unwrap();
    assert_eq!(items(&fresh)[0]["reason"], "broken");
    assert!(fresh.freshness.generation > cached.freshness.generation);
}

#[test]
fn saved_files_survive_busy_or_broken_index() {
    let (_dir, app, root) = setup();
    let token = Cancellation::default();
    let initial = app.rebuild_index(&token).unwrap();
    let index_path = Index::cache_path(&app.vault).unwrap();
    let other = rusqlite::Connection::open(&index_path).unwrap();
    other.execute_batch("BEGIN IMMEDIATE").unwrap();
    let draft = app.capture("Сохранено", "важный текст").unwrap();
    assert!(draft.saved);
    assert!(draft.warnings.iter().any(|w| w.code == "index_stale"));
    assert_eq!(
        app.clone()
            .with_freshness(FreshnessMode::Cached)
            .catalog(&token)
            .unwrap()
            .freshness
            .generation,
        initial.freshness.generation
    );
    other.execute_batch("ROLLBACK").unwrap();
    drop(other);
    let mut index = Index::open(&app.vault).unwrap();
    index
        .reconcile(&app.vault, &app.config, FreshnessMode::Stat, &token)
        .unwrap();
    drop(index);
    fs::remove_file(&index_path).unwrap();
    fs::create_dir(&index_path).unwrap();
    let body = app.raw(&identity(&draft)).unwrap();
    assert!(String::from_utf8(body).unwrap().contains("важный текст"));
    assert!(App::doctor_at(app.vault.root(), None, None).is_ok());
    let plan = {
        let guard = app.vault.lock().unwrap();
        let root_record = app.scan().unwrap().find(&root).unwrap().clone();
        app.vault.begin_edit(&guard, &root_record.path).unwrap()
    };
    let recovered = App::doctor_at(
        app.vault.root(),
        Some(&plan.operation_id),
        Some(zetto::vault::RecoveryMode::Resume),
    )
    .unwrap();
    assert!(recovered.saved);
    assert!(recovered.warnings.iter().any(|w| w.code == "index_stale"));
    let saved = app.capture("", "не потерять").unwrap();
    assert!(saved.saved && saved.warnings.iter().any(|w| w.code == "index_stale"));
    let error = app.publish(&identity(&saved), Some(&root)).unwrap_err();
    assert!(error.saved);
    assert!(
        error
            .context
            .warnings
            .iter()
            .any(|w| w.code == "index_stale")
    );
    fs::remove_dir(&index_path).unwrap();
}

#[test]
fn cached_show_and_preview_have_explicit_content_sources() {
    let (_dir, app, root) = setup();
    let token = Cancellation::default();
    let first = app.show(&root).unwrap();
    let path = first.data["path"].as_str().unwrap();
    let mut raw = String::from_utf8(app.vault.read(path).unwrap()).unwrap();
    raw.push_str("Новая версия тела\n");
    fs::write(app.vault.path(path).unwrap(), raw).unwrap();
    let cached = app
        .clone()
        .with_freshness(FreshnessMode::Cached)
        .show(&root)
        .unwrap();
    assert_eq!(cached.data["body"], first.data["body"]);
    assert_eq!(cached.freshness.generation, first.freshness.generation);
    assert_eq!(cached.freshness.mode, FreshnessMode::Cached);
    assert!(
        app.preview(path, &token)
            .unwrap()
            .note
            .body
            .contains("Новая версия тела")
    );
    let fresh = app.show(&root).unwrap();
    assert!(
        fresh.data["body"]
            .as_str()
            .unwrap()
            .contains("Новая версия тела")
    );
    assert!(fresh.freshness.generation > first.freshness.generation);
    let cancelled = Cancellation::default();
    cancelled.cancel();
    assert_eq!(
        app.search("мысль", SearchMode::Default, 50, 0, &cancelled)
            .unwrap_err()
            .code,
        "interrupted"
    );
}
