use serde_json::Value;
use std::process::{Command, Output, Stdio};
use tempfile::TempDir;

fn cli(vault: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_zetto"))
        .arg("--vault")
        .arg(vault)
        .arg("--format=json")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[cfg(unix)]
#[test]
fn invalid_utf8_vault_path_is_rejected_before_creating_notes() {
    use std::os::unix::ffi::OsStrExt;
    let temp = TempDir::new().unwrap();
    let path = temp.path().join(std::ffi::OsStr::from_bytes(b"bad-\xff"));
    let out = cli(&path, &["init"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert_eq!(json(&out)["error"]["code"], "invalid_config");
    assert!(!path.exists());
}
fn json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {:?}", out))
}

#[test]
fn query_commands_expose_generation_and_machine_formats() {
    let temp = TempDir::new().unwrap();
    let root = json(&cli(temp.path(), &["init"]))["data"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let draft = json(&cli(
        temp.path(),
        &[
            "capture",
            "alpha.*beta #мысль `#код`",
            "--title",
            "Кириллица",
        ],
    ));
    let id = draft["data"]["id"].as_str().unwrap();
    assert!(cli(temp.path(), &["link", id, &root]).status.success());
    assert!(cli(temp.path(), &["publish", id]).status.success());
    assert!(
        cli(temp.path(), &["capture", "alphaXYZbeta alpha"])
            .status
            .success()
    );
    let searched = cli(temp.path(), &["search", "alpha", "--limit=1"]);
    assert!(searched.status.success(), "{searched:?}");
    let searched = json(&searched);
    assert_eq!(searched["freshness"]["mode"], "stat");
    assert!(searched["freshness"]["generation"].is_i64());
    assert_eq!(searched["truncated"], true);
    assert_eq!(searched["data"]["next_offset"], 1);
    let next = json(&cli(
        temp.path(),
        &["search", "alpha", "--limit=1", "--offset=1"],
    ));
    assert_ne!(
        searched["data"]["items"][0]["id"],
        next["data"]["items"][0]["id"]
    );
    assert_eq!(
        json(&cli(temp.path(), &["search", "alpha.*beta", "--literal"]))["data"]["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        json(&cli(temp.path(), &["search", "alpha.*beta", "--regex"]))["data"]["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        json(&cli(temp.path(), &["search", "Кириллица", "--fts"]))["data"]["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    for args in [
        vec!["search", "[", "--regex"],
        vec!["search", "\"", "--fts"],
        vec!["search", "x", "--literal", "--regex"],
        vec!["graph", id, "--depth=4"],
        vec!["--fresh", "--cached", "list"],
    ] {
        let out = cli(temp.path(), &args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {out:?}");
        assert_eq!(json(&out)["error"]["code"], "usage");
    }
    let links = json(&cli(temp.path(), &["links", &root, "--direction=incoming"]));
    assert_eq!(links["data"]["items"][0]["source_id"], id);
    assert_eq!(links["data"]["items"][0]["resolved_id"], root);
    let graph = json(&cli(temp.path(), &["graph", &root]));
    assert_eq!(graph["data"]["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(
        json(&cli(temp.path(), &["tags"]))["data"]["items"],
        serde_json::json!([{"tag":"мысль","count":1}])
    );
    let rebuilt = json(&cli(temp.path(), &["index", "rebuild"]));
    assert_eq!(rebuilt["freshness"]["mode"], "full");
    let generation = rebuilt["freshness"]["generation"].clone();
    assert!(
        cli(temp.path(), &["capture", "cached later"])
            .status
            .success()
    );
    let cached = json(&cli(temp.path(), &["--cached", "search", "later"]));
    assert_eq!(cached["freshness"]["mode"], "cached");
    assert_eq!(cached["freshness"]["generation"], generation);
    assert!(cached["data"]["items"].as_array().unwrap().is_empty());
    assert!(
        cached["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "index_stale")
    );
    let fresh = json(&cli(temp.path(), &["--fresh", "search", "later"]));
    assert_eq!(fresh["freshness"]["mode"], "full");
    assert_eq!(fresh["data"]["items"].as_array().unwrap().len(), 1);
    for args in [
        vec!["search", "нет-выдачи"],
        vec!["links", &root],
        vec!["graph", &root],
        vec!["tags"],
        vec!["path", &root],
        vec!["index", "rebuild"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_zetto"))
            .args(["--vault", temp.path().to_str().unwrap(), "--format=jsonl"])
            .args(&args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {out:?}");
        let rows: Vec<Value> = String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows.last().unwrap()["type"], "summary");
        assert!(rows.last().unwrap()["data"]["freshness"]["generation"].is_i64());
    }
}

#[test]
fn paths_are_plain_or_nul_and_cached_is_visible_to_people() {
    let temp = TempDir::new().unwrap();
    let vault_path = temp.path().join("Заметки 東京");
    let root = json(&cli(&vault_path, &["init"]))["data"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let missing = cli(&vault_path, &["--cached", "list"]);
    assert_eq!(missing.status.code(), Some(4));
    assert_eq!(json(&missing)["error"]["code"], "index_unavailable");
    let record = json(&cli(&vault_path, &["show", &root]));
    let path = std::fs::canonicalize(&vault_path)
        .unwrap()
        .join(record["data"]["path"].as_str().unwrap());
    for (suffix, separator) in [(vec![], b'\n'), (vec!["--null"], 0)] {
        let out = Command::new(env!("CARGO_BIN_EXE_zetto"))
            .arg("--vault")
            .arg(&vault_path)
            .args(["path", &root])
            .args(suffix)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        let mut expected = path.to_str().unwrap().as_bytes().to_vec();
        expected.push(separator);
        assert_eq!(out.stdout, expected);
    }
    assert_eq!(
        cli(&vault_path, &["path", &root, "--null"]).status.code(),
        Some(2)
    );
    let out = Command::new(env!("CARGO_BIN_EXE_zetto"))
        .arg("--vault")
        .arg(&vault_path)
        .args(["--cached", "list"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8(out.stderr).unwrap().contains("cached"));
    cli(&vault_path, &["capture", "#мысль"]);
    let out = Command::new(env!("CARGO_BIN_EXE_zetto"))
        .arg("--vault")
        .arg(&vault_path)
        .arg("tags")
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("ТЕГ") && text.contains("мысль\t1"), "{text}");
}

#[test]
fn index_warnings_survive_saved_errors_and_new_flow() {
    let temp = TempDir::new().unwrap();
    let root = json(&cli(temp.path(), &["init"]))["data"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    cli(temp.path(), &["list"]);
    let vault = zetto::vault::Vault::open(temp.path()).unwrap();
    let index =
        rusqlite::Connection::open(zetto::index::Index::cache_path(&vault).unwrap()).unwrap();
    index.execute_batch("BEGIN IMMEDIATE").unwrap();
    let out = cli(
        temp.path(),
        &["new", "сохранить даже при ошибке", "--link", &root],
    );
    assert_eq!(out.status.code(), Some(1));
    let result = json(&out);
    assert_eq!(result["saved"], true);
    assert!(
        result["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "index_stale"),
        "{result}"
    );
    assert_eq!(result["freshness"]["mode"], "not_applicable");
    let saved = result["data"]["path"].as_str().unwrap();
    assert!(
        std::fs::read_to_string(temp.path().join(saved))
            .unwrap()
            .contains("сохранить даже при ошибке")
    );
    index.execute_batch("ROLLBACK").unwrap();
}
#[test]
fn cycle_and_saved_draft() {
    let temp = TempDir::new().unwrap();
    let vault = temp.path().join("notes");
    let root = cli(&vault, &["init"]);
    assert!(root.status.success(), "{root:?}");
    let root = json(&root)["data"]["id"].as_str().unwrap().to_owned();
    let draft = cli(&vault, &["capture", "тело\r\n", "--title", "Мысль"]);
    assert!(draft.status.success(), "{draft:?}");
    let draft = json(&draft);
    let id = draft["data"]["id"].as_str().unwrap();
    assert_eq!(
        json(&cli(&vault, &["show", id]))["data"]["body"],
        "тело\r\n"
    );
    assert!(cli(&vault, &["link", id, &root]).status.success());
    assert!(cli(&vault, &["publish", id]).status.success());
    let failed = cli(&vault, &["new", "сохранить\n", "--title", "Другая"]);
    assert_eq!(failed.status.code(), Some(1));
    let failed = json(&failed);
    assert_eq!(failed["status"], "draft_saved");
    assert_eq!(failed["saved"], true);
    assert_eq!(
        json(&cli(
            &vault,
            &["show", failed["data"]["id"].as_str().unwrap()]
        ))["data"]["body"],
        "сохранить\n"
    );
}

#[cfg(unix)]
#[test]
fn publish_io_failure_retains_saved_draft_and_recovery() {
    use std::os::unix::fs::PermissionsExt;
    let temp = TempDir::new().unwrap();
    let root = json(&cli(temp.path(), &["init"]))["data"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let draft = json(&cli(temp.path(), &["capture", "Текст", "--title", "Мысль"]));
    let id = draft["data"]["id"].as_str().unwrap();
    let path = draft["data"]["path"].as_str().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let out = cli(temp.path(), &["publish", id, "--link", &root]);
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(out.status.code(), Some(4));
    let failed = json(&out);
    assert_eq!(failed["error"]["code"], "io");
    assert_eq!(failed["saved"], true);
    assert_eq!(failed["status"], "draft_saved");
    assert_eq!(failed["data"]["id"], id);
    assert_eq!(failed["data"]["path"], path);
    let recovery = failed["recovery"].as_array().unwrap();
    assert!(!recovery.is_empty());
    assert!(
        recovery
            .iter()
            .all(|p| temp.path().join(p.as_str().unwrap()).exists())
    );
    assert!(
        std::fs::read_to_string(temp.path().join(path))
            .unwrap()
            .contains(&format!("[[{root}]]"))
    );
    let listed = json(&cli(temp.path(), &["list", "--stage=all"]));
    assert_eq!(listed["data"]["items"].as_array().unwrap().len(), 2);
}

#[test]
fn list_of_fully_read_invalid_note_is_complete() {
    let temp = TempDir::new().unwrap();
    assert!(cli(temp.path(), &["init"]).status.success());
    std::fs::write(temp.path().join("bad.md"), "---\ntitle: [bad\n---\nТекст").unwrap();
    let out = cli(temp.path(), &["list"]);
    assert!(out.status.success());
    let listed = json(&out);
    let invalid = listed["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["path"] == "bad.md")
        .unwrap();
    assert!(invalid["id"].is_null());
    assert!(
        listed["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["path"] == "bad.md")
    );
    assert_eq!(listed["complete"], true);
    assert_eq!(listed["truncated"], false);
    std::fs::write(temp.path().join("bad.md"), [0xff]).unwrap();
    let unreadable = json(&cli(temp.path(), &["list"]));
    assert_eq!(unreadable["complete"], false);
    assert_eq!(unreadable["truncated"], false);
    assert!(
        unreadable["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| { d["path"] == "bad.md" && d["rule_id"] == "zetto/file-read-error" })
    );
}

#[test]
fn syntax_and_non_tty_are_structured() {
    let temp = TempDir::new().unwrap();
    for args in [
        vec!["wat"],
        vec!["list", "--limit", "0"],
        vec!["show", "../../evil"],
        vec!["capture"],
        vec!["edit", "01ARZ3NDEKTSV4RRFFQ69G5FAV"],
        vec![],
    ] {
        let out = cli(temp.path(), &args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {out:?}");
        assert_eq!(json(&out)["status"], "error");
    }
    let raw = cli(
        temp.path(),
        &["show", "01ARZ3NDEKTSV4RRFFQ69G5FAV", "--raw"],
    );
    assert_eq!(json(&raw)["error"]["code"], "usage");
}

#[test]
fn jsonl_always_ends_with_summary() {
    let temp = TempDir::new().unwrap();
    assert!(cli(temp.path(), &["init"]).status.success());
    for (args, expected_count, exit) in [
        (vec!["list", "--stage=inbox"], 0, 0),
        (vec!["list"], 1, 0),
        (vec!["list", "--bad"], 0, 2),
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_zetto"))
            .arg("--vault")
            .arg(temp.path())
            .arg("--format=jsonl")
            .args(args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(exit), "{out:?}");
        assert!(out.stdout.ends_with(b"\n"));
        let rows: Vec<Value> = String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows.last().unwrap()["type"], "summary");
        assert_eq!(rows.last().unwrap()["data"]["count"], expected_count);
    }
}

#[test]
fn explicit_inputs_preserve_bytes_and_reject_invalid_data_before_write() {
    use std::io::Write;
    let temp = TempDir::new().unwrap();
    assert!(cli(temp.path(), &["init"]).status.success());
    let body = "первая\r\nвторая\n\n";
    let file = temp.path().join("input.txt");
    std::fs::write(&file, body).unwrap();
    let captured = cli(temp.path(), &["capture", "--file", file.to_str().unwrap()]);
    let captured = json(&captured);
    let id = captured["data"]["id"].as_str().unwrap();
    assert_eq!(json(&cli(temp.path(), &["show", id]))["data"]["body"], body);
    let raw = Command::new(env!("CARGO_BIN_EXE_zetto"))
        .arg("--vault")
        .arg(temp.path())
        .args(["show", id, "--raw"])
        .output()
        .unwrap();
    assert!(raw.status.success());
    assert_eq!(
        raw.stdout,
        std::fs::read(temp.path().join(captured["data"]["path"].as_str().unwrap())).unwrap()
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_zetto"))
        .arg("--vault")
        .arg(temp.path())
        .args(["--format=json", "capture", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(body.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let value = json(&out);
    assert_eq!(
        json(&cli(
            temp.path(),
            &["show", value["data"]["id"].as_str().unwrap()]
        ))["data"]["body"],
        body
    );
    for bytes in [vec![0xff], vec![b'x'; 16 * 1024 * 1024 + 1]] {
        std::fs::write(&file, bytes).unwrap();
        let out = cli(temp.path(), &["capture", "--file", file.to_str().unwrap()]);
        assert_eq!(json(&out)["error"]["code"], "validation_failed");
        assert_eq!(json(&out)["saved"], false);
    }
    assert_eq!(
        std::fs::read_dir(temp.path().join("inbox"))
            .unwrap()
            .count(),
        2
    );
    let empty = cli(temp.path(), &["capture", ""]);
    assert!(empty.status.success());
    assert_eq!(
        json(&cli(
            temp.path(),
            &["show", json(&empty)["data"]["id"].as_str().unwrap()]
        ))["data"]["body"],
        ""
    );
}

#[test]
fn pipe_is_never_an_implicit_source() {
    use std::io::Write;
    let temp = TempDir::new().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_zetto"))
        .arg("--vault")
        .arg(temp.path())
        .args(["--format=json", "capture"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let _ = child.stdin.take().unwrap().write_all(b"must not be read");
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json(&out)["error"]["code"], "interaction_required");
    assert!(!temp.path().join("inbox").exists());
}

#[test]
fn discovery_and_editor_settings_have_no_hidden_execution() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let default = home.join(".zetto/notes");
    let env_vault = temp.path().join("environment");
    let config = temp.path().join("config/zetto");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("config.toml"),
        "editor = [\"definitely-not-an-editor\"]\n",
    )
    .unwrap();
    let call = |args: &[&str], cwd: &std::path::Path, vault: Option<&std::path::Path>| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_zetto"));
        command
            .args(["--format=json"])
            .args(args)
            .current_dir(cwd)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .env_remove("ZETTO_VAULT")
            .stdin(Stdio::null());
        if let Some(vault) = vault {
            command.env("ZETTO_VAULT", vault);
        }
        command.output().unwrap()
    };
    assert!(call(&["init"], temp.path(), None).status.success());
    assert!(default.join(".zettorc").exists());
    assert!(
        call(&["init"], temp.path(), Some(&env_vault))
            .status
            .success()
    );
    assert!(env_vault.join(".zettorc").exists());
    let nested = env_vault.join("assets/nested");
    std::fs::create_dir(&nested).unwrap();
    assert!(call(&["list"], &nested, None).status.success());
    let out = call(
        &["--vault", default.to_str().unwrap(), "list"],
        &nested,
        Some(&env_vault),
    );
    assert!(out.status.success());
    let default_root = json(&cli(&default, &["list"]))["data"]["items"][0]["id"].clone();
    assert_eq!(json(&out)["data"]["items"][0]["id"], default_root);
    let out = cli(
        &default,
        &["init", temp.path().join("other").to_str().unwrap()],
    );
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn pagination_and_doctor_are_exposed() {
    let temp = TempDir::new().unwrap();
    assert!(cli(temp.path(), &["init"]).status.success());
    for body in ["one", "two"] {
        assert!(cli(temp.path(), &["capture", body]).status.success());
    }
    let page = json(&cli(temp.path(), &["list", "--stage=inbox", "--limit=1"]));
    assert_eq!(page["truncated"], true);
    assert_eq!(page["complete"], false);
    assert_eq!(page["data"]["next_offset"], 1);
    assert_eq!(
        json(&cli(temp.path(), &["list", "--stage=inbox", "--offset=2"]))["data"]["items"],
        serde_json::json!([])
    );
    assert!(cli(temp.path(), &["doctor"]).status.success());
    let root = json(&cli(temp.path(), &["list"]))["data"]["items"][0].clone();
    let vault = zetto::vault::Vault::open(temp.path()).unwrap();
    for mode in ["--resume", "--restore"] {
        let guard = vault.lock().unwrap();
        let plan = vault
            .begin_edit(&guard, root["path"].as_str().unwrap())
            .unwrap();
        drop(guard);
        assert_eq!(
            json(&cli(temp.path(), &["doctor"]))["data"]["operations"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let out = cli(
            temp.path(),
            &["doctor", "--operation", &plan.operation_id, mode],
        );
        assert!(out.status.success(), "{out:?}");
        assert!(
            json(&cli(temp.path(), &["doctor"]))["data"]["operations"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    assert_eq!(
        cli(temp.path(), &["doctor", "--resume"]).status.code(),
        Some(2)
    );
}

#[test]
fn occupied_writer_lock_refuses_cli_without_overwrite() {
    let temp = TempDir::new().unwrap();
    assert!(cli(temp.path(), &["init"]).status.success());
    let vault = zetto::vault::Vault::open(temp.path()).unwrap();
    let guard = vault.lock().unwrap();
    for _ in 0..2 {
        let out = cli(temp.path(), &["capture", "must not be written"]);
        assert_eq!(out.status.code(), Some(3));
        assert_eq!(json(&out)["error"]["code"], "lock_busy");
    }
    assert_eq!(
        std::fs::read_dir(temp.path().join("inbox"))
            .unwrap()
            .count(),
        0
    );
    drop(guard);
    assert!(
        cli(temp.path(), &["capture", "after unlock"])
            .status
            .success()
    );
}

#[cfg(unix)]
#[test]
fn sigint_saves_received_stdin_and_does_not_wait_for_eof() {
    use std::io::Write;
    let temp = TempDir::new().unwrap();
    assert!(cli(temp.path(), &["init"]).status.success());
    let mut child = Command::new(env!("CARGO_BIN_EXE_zetto"))
        .arg("--vault")
        .arg(temp.path())
        .args(["--format=json", "new", "--stdin", "--title", "Прервано"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    input.write_all("принято\n".as_bytes()).unwrap();
    // Даём обработчику установиться и читателю принять байты; stdin остаётся открыт.
    std::thread::sleep(std::time::Duration::from_millis(150));
    assert!(
        Command::new("kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    let start = std::time::Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(3),
            "SIGINT завис на stdin"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    drop(input);
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(130), "{out:?}");
    let value = json(&out);
    assert_eq!(value["saved"], true);
    assert_eq!(value["error"]["code"], "interrupted");
    let show = json(&cli(
        temp.path(),
        &["show", value["data"]["id"].as_str().unwrap()],
    ));
    assert_eq!(show["data"]["stage"], "inbox");
    assert_eq!(show["data"]["body"], "принято\n");
}

#[cfg(unix)]
#[test]
fn broken_pipe_exits_four_after_one_save() {
    let temp = TempDir::new().unwrap();
    assert!(cli(temp.path(), &["init"]).status.success());
    let mut child = Command::new(env!("CARGO_BIN_EXE_zetto"))
        .arg("--vault")
        .arg(temp.path())
        .args(["--format=json", "capture", "saved once"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    assert_eq!(child.wait().unwrap().code(), Some(4));
    assert_eq!(
        std::fs::read_dir(temp.path().join("inbox"))
            .unwrap()
            .count(),
        1
    );
}
