use std::fs;
use tempfile::tempdir;
use zetto::vault::{RecoveryMode, Vault, hash};

const ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

fn setup() -> (tempfile::TempDir, Vault, String) {
    let dir = tempdir().unwrap();
    let root = dir.path().join("vault");
    let name = format!("{ID}-root.md");
    let bytes = format!("---\nid: {ID}\ntitle: Root\n---\nRoot\n");
    let config = format!("config_version = 1\nformat_version = 1\nroot_id = \"{ID}\"\n");
    let vault = Vault::initialize(&root, &name, bytes.as_bytes(), &config).unwrap();
    (dir, vault, name)
}

#[test]
fn safe_paths_lock_and_creation_never_overwrite() {
    let (_dir, vault, name) = setup();
    for path in ["../escape.md", "/tmp/escape.md", "inbox/../escape.md", ""] {
        assert!(vault.path(path).is_err(), "{path}");
    }
    let guard = vault.lock().unwrap();
    assert_eq!(vault.lock().unwrap_err().code, "lock_busy");
    assert_eq!(
        vault.create(&guard, &name, b"overwrite").unwrap_err().code,
        "target_exists"
    );
    assert!(
        String::from_utf8(vault.read(&name).unwrap())
            .unwrap()
            .contains("Root")
    );
    drop(guard);
    assert!(vault.lock().is_ok());
}

#[test]
fn replacement_preserves_backup_and_conflicting_work() {
    let (_dir, vault, name) = setup();
    let base = vault.read(&name).unwrap();
    let guard = vault.lock().unwrap();
    let plan = vault
        .replace(&guard, &name, &hash(&base), b"new bytes")
        .unwrap();
    assert_eq!(vault.read(&name).unwrap(), b"new bytes");
    assert_eq!(vault.read(&plan.backup).unwrap(), base);
    let error = vault
        .replace(&guard, &name, &hash(&base), b"conflicting text")
        .unwrap_err();
    assert_eq!(error.code, "conflict");
    assert_eq!(vault.read(&name).unwrap(), b"new bytes");
    assert!(
        error
            .recovery
            .iter()
            .any(|p| vault.read(p).is_ok_and(|b| b == b"conflicting text"))
    );
}

#[test]
fn editor_work_does_not_hold_lock_and_reads_replaced_path() {
    let (_dir, vault, name) = setup();
    let guard = vault.lock().unwrap();
    let plan = vault.begin_edit(&guard, &name).unwrap();
    drop(guard);
    let guard = vault.lock().unwrap();
    let work = vault.path(&plan.work).unwrap();
    let replacement = work.with_extension("replacement");
    fs::write(&replacement, b"editor replaced the inode").unwrap();
    fs::rename(replacement, work).unwrap();
    vault
        .finish_edit(&guard, &plan.operation_id, b"editor replaced the inode")
        .unwrap();
    assert_eq!(vault.read(&name).unwrap(), b"editor replaced the inode");
}

#[test]
fn editor_conflict_retains_both_versions_and_can_abandon_work() {
    let (_dir, vault, name) = setup();
    let guard = vault.lock().unwrap();
    let plan = vault.begin_edit(&guard, &name).unwrap();
    fs::write(vault.path(&name).unwrap(), b"external version").unwrap();
    let error = vault
        .finish_edit(&guard, &plan.operation_id, b"editor version")
        .unwrap_err();
    assert_eq!(error.code, "conflict");
    assert_eq!(vault.read(&name).unwrap(), b"external version");
    assert_eq!(vault.read(&plan.work).unwrap(), b"editor version");
    vault
        .recover(&guard, &plan.operation_id, RecoveryMode::Restore)
        .unwrap();
    assert_eq!(vault.read(&name).unwrap(), b"external version");
    assert_eq!(vault.read(&plan.work).unwrap(), b"editor version");
}

#[test]
fn recovery_plan_paths_are_untrusted() {
    let (dir, vault, name) = setup();
    let guard = vault.lock().unwrap();
    let plan = vault.begin_edit(&guard, &name).unwrap();
    let path = vault
        .path(&format!(".zetto/recovery/{}/plan.json", plan.operation_id))
        .unwrap();
    let mut value = serde_json::to_value(&plan).unwrap();
    value["from"] = serde_json::json!("../outside.md");
    fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(
        vault
            .recover(&guard, &plan.operation_id, RecoveryMode::Resume)
            .is_err()
    );
    assert!(!dir.path().join("outside.md").exists());
}

#[cfg(unix)]
#[test]
fn special_files_are_rejected_before_opening() {
    let (_dir, vault, _name) = setup();
    let _socket = std::os::unix::net::UnixListener::bind(vault.root().join("socket.md")).unwrap();
    assert_eq!(
        vault.read("socket.md").unwrap_err().code,
        "validation_failed"
    );
}

#[cfg(unix)]
#[test]
fn interrupted_restore_is_pending_and_resume_finishes_undo() {
    use std::os::unix::fs::PermissionsExt;
    let (_dir, vault, _name) = setup();
    let guard = vault.lock().unwrap();
    vault.create(&guard, "inbox/draft.md", b"text").unwrap();
    let plan = vault
        .publish(&guard, "inbox/draft.md", "published.md", &hash(b"text"))
        .unwrap();
    fs::set_permissions(vault.root(), fs::Permissions::from_mode(0o500)).unwrap();
    let result = vault.recover(&guard, &plan.operation_id, RecoveryMode::Restore);
    fs::set_permissions(vault.root(), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.is_err());
    assert!(vault.plan(&plan.operation_id).unwrap().unfinished());
    vault
        .recover(&guard, &plan.operation_id, RecoveryMode::Resume)
        .unwrap();
    assert_eq!(vault.read("inbox/draft.md").unwrap(), b"text");
    assert!(!vault.path("published.md").unwrap().exists());
}

#[test]
fn publish_is_recoverable_and_no_clobber() {
    let (_dir, vault, _name) = setup();
    let guard = vault.lock().unwrap();
    let from = "inbox/draft.md";
    vault.create(&guard, from, b"draft").unwrap();
    vault.create(&guard, "existing.md", b"existing").unwrap();
    assert_eq!(
        vault
            .publish(&guard, from, "existing.md", &hash(b"draft"))
            .unwrap_err()
            .code,
        "target_exists"
    );
    assert_eq!(vault.read(from).unwrap(), b"draft");
    assert_eq!(vault.read("existing.md").unwrap(), b"existing");
    let plan = vault
        .publish(&guard, from, "published.md", &hash(b"draft"))
        .unwrap();
    assert_eq!(vault.read("published.md").unwrap(), b"draft");
    assert!(!vault.path(from).unwrap().exists());
    vault
        .recover(&guard, &plan.operation_id, RecoveryMode::Resume)
        .unwrap();
}

#[cfg(unix)]
#[test]
fn symlink_components_are_rejected_in_notes_and_recovery() {
    use std::os::unix::fs::symlink;
    let (dir, vault, _name) = setup();
    let outside = dir.path().join("outside");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, vault.root().join("inbox/link")).unwrap();
    assert!(vault.path("inbox/link/escape.md").is_err());
    symlink(&outside, vault.root().join(".zetto/recovery/unsafe")).unwrap();
    assert!(vault.plans().is_err());
}

#[cfg(unix)]
#[test]
fn write_permission_failure_keeps_original() {
    use std::os::unix::fs::PermissionsExt;
    let (_dir, vault, name) = setup();
    let guard = vault.lock().unwrap();
    let original = vault.read(&name).unwrap();
    fs::set_permissions(
        vault.root().join("inbox"),
        fs::Permissions::from_mode(0o500),
    )
    .unwrap();
    let result = vault.create(&guard, "inbox/denied.md", b"text");
    fs::set_permissions(
        vault.root().join("inbox"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    assert!(result.is_err());
    assert_eq!(vault.read(&name).unwrap(), original);
    assert!(!vault.path("inbox/denied.md").unwrap().exists());
}
