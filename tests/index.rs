use serde_json::{Value, json};
use std::fs::{self, File, FileTimes};
use std::time::{Duration, Instant};
use tempfile::TempDir;
use zetto::app::{App, Catalog};
use zetto::index::{Cancellation, FreshnessMode, Index};

fn setup() -> (TempDir, App, Index, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vault");
    let init = App::init(&path, "Корень").unwrap();
    let app = App::open(&path).unwrap();
    let cache_path = app
        .vault
        .root()
        .parent()
        .unwrap()
        .join("cache/index.sqlite3");
    let index = Index::open_at(&app.vault, &cache_path).unwrap();
    (dir, app, index, init.data["id"].as_str().unwrap().into())
}

fn graph(catalog: &Catalog) -> Vec<Value> {
    catalog.records.iter().map(|r| r.data()).collect()
}

fn edges(index: &Index) -> Value {
    serde_json::to_value(index.links().unwrap()).unwrap()
}

#[test]
fn incremental_and_full_match_after_external_changes() {
    let (_dir, app, mut index, root) = setup();
    let draft = app
        .capture(
            "Источник",
            &format!("[[{root}]] [[псевдоним]] #мысль `#код`"),
        )
        .unwrap();
    let token = Cancellation::default();
    let first = index
        .reconcile(&app.vault, &app.config, FreshnessMode::Stat, &token)
        .unwrap();
    assert_eq!(graph(&first), graph(&app.scan().unwrap()));
    assert!(
        first
            .records
            .iter()
            .all(|r| r.raw.is_none() && r.note.body.is_empty())
    );
    assert_eq!(first.metrics.files_read, 2);
    let unchanged = index
        .reconcile(&app.vault, &app.config, FreshnessMode::Stat, &token)
        .unwrap();
    assert_eq!(unchanged.metrics.files_read, 0);
    assert_eq!(first.freshness.generation, unchanged.freshness.generation);
    let root_record = app.scan().unwrap().find(&root).unwrap().clone();
    let mut fields = root_record.note.fields;
    fields.insert("aliases".into(), json!(["псевдоним"]));
    fields.insert("title".into(), json!("Новое имя"));
    let renamed = zetto::format::filename(&root, "Новое имя");
    fs::write(
        app.vault.path(&renamed).unwrap(),
        zetto::format::render(&fields, "Новый текст").unwrap(),
    )
    .unwrap();
    fs::remove_file(app.vault.path(&root_record.path).unwrap()).unwrap();
    let incremental = index
        .reconcile(&app.vault, &app.config, FreshnessMode::Stat, &token)
        .unwrap();
    assert_eq!(graph(&incremental), graph(&app.scan().unwrap()));
    let incremental_edges = edges(&index);
    assert_eq!(incremental_edges[1]["reason"], "resolved");
    let rebuilt = index.rebuild(&app.vault, &app.config, &token).unwrap();
    assert_eq!(graph(&incremental), graph(&rebuilt));
    assert_eq!(incremental_edges, edges(&index));
    let duplicate = format!("{root}-duplicate.md");
    fs::copy(
        app.vault.path(&renamed).unwrap(),
        app.vault.path(&duplicate).unwrap(),
    )
    .unwrap();
    fs::write(
        app.vault.path("broken.md").unwrap(),
        "---\nid: [unterminated\n---\ntext",
    )
    .unwrap();
    let broken = index
        .reconcile(&app.vault, &app.config, FreshnessMode::Stat, &token)
        .unwrap();
    assert_eq!(graph(&broken), graph(&app.scan().unwrap()));
    assert!(broken.find(&root).is_err());
    fs::remove_file(app.vault.path(&renamed).unwrap()).unwrap();
    fs::remove_file(app.vault.path(&duplicate).unwrap()).unwrap();
    let removed = index
        .reconcile(&app.vault, &app.config, FreshnessMode::Stat, &token)
        .unwrap();
    assert_eq!(graph(&removed), graph(&app.scan().unwrap()));
    assert!(
        removed
            .find(draft.data["id"].as_str().unwrap())
            .unwrap()
            .tags
            .contains(&"мысль".into())
    );
    assert!(
        !removed
            .find(draft.data["id"].as_str().unwrap())
            .unwrap()
            .tags
            .contains(&"код".into())
    );
}

#[test]
fn alias_collision_and_asset_lifecycle_refresh_persisted_edges() {
    let (_dir, app, mut index, root) = setup();
    let token = Cancellation::default();
    let original = app.scan().unwrap().find(&root).unwrap().clone();
    let mut fields = original.note.fields;
    fields.insert("aliases".into(), json!(["Общий"]));
    let root_raw = zetto::format::render(&fields, "[вложение](assets/test.txt)").unwrap();
    fs::write(app.vault.path(&original.path).unwrap(), root_raw).unwrap();
    app.capture("Источник", "[[Общий]]").unwrap();
    let first = index.rebuild(&app.vault, &app.config, &token).unwrap();
    assert!(index.links().unwrap().iter().any(|l| l.reason == "broken"));
    fs::write(app.vault.path("assets/test.txt").unwrap(), "asset").unwrap();
    let attached = index
        .reconcile(&app.vault, &app.config, FreshnessMode::Stat, &token)
        .unwrap();
    assert_eq!(attached.metrics.files_read, 0);
    assert!(attached.freshness.generation > first.freshness.generation);
    assert!(
        index
            .links()
            .unwrap()
            .iter()
            .any(|l| l.reason == "attachment")
    );
    assert_eq!(graph(&attached), graph(&index.snapshot().unwrap()));
    let second = app.capture("Конкурент", "").unwrap();
    let second_path = second.data["path"].as_str().unwrap();
    let mut note =
        zetto::format::parse(&String::from_utf8(app.vault.read(second_path).unwrap()).unwrap());
    note.fields.insert("aliases".into(), json!(["общий"]));
    fs::write(
        app.vault.path(second_path).unwrap(),
        zetto::format::render(&note.fields, &note.body).unwrap(),
    )
    .unwrap();
    let ambiguous = index
        .reconcile(&app.vault, &app.config, FreshnessMode::Stat, &token)
        .unwrap();
    let links = index.links().unwrap();
    assert!(
        links.iter().any(|l| l.link.target == "Общий"
            && l.reason == "ambiguous"
            && l.target_path.is_none())
    );
    let before = edges(&index);
    assert_eq!(
        graph(&ambiguous),
        graph(&index.rebuild(&app.vault, &app.config, &token).unwrap())
    );
    assert_eq!(before, edges(&index));
    fs::remove_file(app.vault.path("assets/test.txt").unwrap()).unwrap();
    let deleted = index
        .reconcile(&app.vault, &app.config, FreshnessMode::Stat, &token)
        .unwrap();
    assert_eq!(deleted.metrics.files_read, 0);
    assert!(index.links().unwrap().iter().any(|l| l.reason == "broken"));
    assert_eq!(graph(&deleted), graph(&index.snapshot().unwrap()));
}

#[test]
fn inline_tags_use_source_boundaries_and_exclude_code() {
    let tags = zetto::format::inline_tags(
        "#русский #tag\n`#code`\n```\n#fence\n```\nword**#bold** x[#label](#destination) \\#escaped\n#tag",
    );
    assert_eq!(tags, vec!["tag", "русский"]);
}

#[test]
fn cached_is_old_consistent_generation_and_full_checks_same_stat() {
    let (_dir, app, mut index, root) = setup();
    let token = Cancellation::default();
    assert!(index.snapshot().is_err());
    let first = index
        .reconcile(&app.vault, &app.config, FreshnessMode::Full, &token)
        .unwrap();
    let record = app.scan().unwrap().find(&root).unwrap().clone();
    let file = File::open(app.vault.path(&record.path).unwrap()).unwrap();
    let meta = file.metadata().unwrap();
    let raw = record.raw.unwrap();
    let replaced = raw.replace("Корень", "Начало");
    assert_eq!(replaced.len(), raw.len());
    fs::write(app.vault.path(&record.path).unwrap(), replaced).unwrap();
    file.set_times(FileTimes::new().set_modified(meta.modified().unwrap()))
        .unwrap();
    index.mark_dirty().unwrap();
    let cached = index.snapshot().unwrap();
    assert_eq!(cached.freshness.mode, FreshnessMode::Cached);
    assert_eq!(cached.freshness.generation, first.freshness.generation);
    assert!(cached.warnings.iter().any(|w| w.code == "index_stale"));
    assert_eq!(cached.find(&root).unwrap().note.title(), Some("Корень"));
    let stat = index
        .reconcile(&app.vault, &app.config, FreshnessMode::Stat, &token)
        .unwrap();
    assert_eq!(stat.find(&root).unwrap().note.title(), Some("Корень"));
    let full = index
        .reconcile(&app.vault, &app.config, FreshnessMode::Full, &token)
        .unwrap();
    assert_eq!(full.find(&root).unwrap().note.title(), Some("Начало"));
    assert!(full.freshness.generation > first.freshness.generation);
}

#[test]
fn fts_is_bundled_escaped_and_cancellable_and_busy_is_bounded() {
    let (_dir, app, mut index, root) = setup();
    let token = Cancellation::default();
    index.rebuild(&app.vault, &app.config, &token).unwrap();
    assert!(index.sqlite_version().unwrap() >= (3, 51, 3));
    let hits = index.search_fts("Корень", false, 10, 0, &token).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].path.starts_with(&root));
    assert!(
        index
            .search_fts("НЕСУЩЕСТВУЮЩЕЕ + \"", false, 10, 0, &token)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        index
            .search_fts("\"", true, 10, 0, &token)
            .unwrap_err()
            .code,
        "usage"
    );
    let cancelled = Cancellation::default();
    cancelled.cancel();
    assert_eq!(
        index
            .rebuild(&app.vault, &app.config, &cancelled)
            .unwrap_err()
            .code,
        "interrupted"
    );
    let other = rusqlite::Connection::open(index.path()).unwrap();
    other.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert_eq!(index.snapshot().unwrap().freshness.generation, Some(1));
    let start = Instant::now();
    assert_eq!(index.mark_dirty().unwrap_err().code, "index_unavailable");
    assert!(start.elapsed() < Duration::from_secs(2));
    other.execute_batch("ROLLBACK").unwrap();
    index.mark_dirty().unwrap();
}

#[test]
fn invalid_bytes_and_cache_paths_are_explicit_without_touching_notes() {
    let (_dir, app, mut index, _root) = setup();
    let token = Cancellation::default();
    fs::write(app.vault.path("invalid.md").unwrap(), [0xff, 0xfe]).unwrap();
    let catalog = index.rebuild(&app.vault, &app.config, &token).unwrap();
    assert!(!catalog.complete);
    let broken = catalog
        .records
        .iter()
        .find(|r| r.path == "invalid.md")
        .unwrap();
    assert!(!broken.readable);
    assert_eq!(broken.note.diagnostics[0].rule_id, "zetto/file-read-error");
    assert_eq!(app.vault.read("invalid.md").unwrap(), [0xff, 0xfe]);
    assert!(Index::open_at(&app.vault, &app.vault.root().join("index.sqlite3")).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::{PermissionsExt, symlink};
        assert_eq!(
            fs::metadata(index.path().parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(index.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let redirected = index.path().with_file_name("redirected.sqlite3");
        symlink(app.vault.path("invalid.md").unwrap(), &redirected).unwrap();
        assert!(Index::open_at(&app.vault, &redirected).is_err());
        assert_eq!(app.vault.read("invalid.md").unwrap(), [0xff, 0xfe]);
    }
}
