use super::*;

#[test]
fn dropped_guard_unlocks_even_with_transient_inherited_descriptor() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::initialize(&dir.path().join("vault"), "root.md", b"root", "config").unwrap();
    let guard = vault.lock().unwrap();
    let _inherited = guard._file.try_clone().unwrap();
    drop(guard);
    assert!(vault.lock().is_ok());
}

fn fail_sync(path: Option<&Path>, skip: usize) {
    SYNC_FAILURE.with(|failure| *failure.borrow_mut() = path.map(|p| (p.to_path_buf(), skip)));
    SYNC_HITS.with(|hits| hits.borrow_mut().clear());
}

#[test]
fn init_resumes_before_payload_preparation_finishes() {
    for step in ["intent_saved", "backup_written", "work_written"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault");
        FAIL_AFTER.with(|p| p.set(Some(step)));
        let result = Vault::initialize(&path, "root.md", b"root", "config");
        FAIL_AFTER.with(|p| p.set(None));
        assert_eq!(result.unwrap_err().code, "interrupted");
        let vault = Vault::initialize(&path, "other.md", b"other", "other").unwrap();
        assert_eq!(vault.read("root.md").unwrap(), b"root");
        assert_eq!(vault.read(".zettorc").unwrap(), b"config");
    }
}

#[test]
fn preparing_recovery_preserves_payload_and_is_idempotent() {
    for kind in [PlanKind::Replace, PlanKind::Edit, PlanKind::Publish] {
        for step in ["intent_saved", "backup_written", "work_written"] {
            for restore in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let vault =
                    Vault::initialize(&dir.path().join("vault"), "root.md", b"root", "config")
                        .unwrap();
                let guard = vault.lock().unwrap();
                vault.create(&guard, "inbox/draft.md", b"old").unwrap();
                FAIL_AFTER.with(|p| p.set(Some(step)));
                let result = match kind {
                    PlanKind::Replace => {
                        vault.replace(&guard, "inbox/draft.md", &hash(b"old"), b"new")
                    }
                    PlanKind::Edit => vault.begin_edit(&guard, "inbox/draft.md"),
                    PlanKind::Publish => {
                        vault.publish(&guard, "inbox/draft.md", "published.md", &hash(b"old"))
                    }
                    PlanKind::Init => unreachable!(),
                };
                FAIL_AFTER.with(|p| p.set(None));
                assert_eq!(result.unwrap_err().code, "interrupted", "{kind:?}/{step}");
                let plan = vault
                    .plans()
                    .unwrap()
                    .into_iter()
                    .find(|p| p.unfinished())
                    .unwrap();
                assert_eq!(plan.state, PlanState::Preparing);
                assert_eq!(vault.read("inbox/draft.md").unwrap(), b"old");
                let original_backup = (step != "intent_saved").then(|| b"old".to_vec());
                let proposed: &[u8] = if kind == PlanKind::Replace {
                    b"new"
                } else {
                    b"old"
                };
                let original_work = (step == "work_written").then(|| proposed.to_vec());
                assert_eq!(vault.read_optional(&plan.backup).unwrap(), original_backup);
                assert_eq!(vault.read_optional(&plan.work).unwrap(), original_work);
                let missing_new = kind == PlanKind::Replace && step != "work_written";
                let undo = restore || missing_new;
                if missing_new && !restore {
                    for _ in 0..2 {
                        assert_eq!(
                            vault
                                .recover(&guard, &plan.operation_id, RecoveryMode::Resume)
                                .unwrap_err()
                                .code,
                            "recovery_required"
                        );
                        assert!(vault.plan(&plan.operation_id).unwrap().unfinished());
                    }
                }
                let mode = if undo {
                    RecoveryMode::Restore
                } else {
                    RecoveryMode::Resume
                };
                for _ in 0..2 {
                    vault.recover(&guard, &plan.operation_id, mode).unwrap();
                }
                assert_eq!(
                    vault.plan(&plan.operation_id).unwrap().state,
                    if undo {
                        PlanState::Abandoned
                    } else {
                        PlanState::Complete
                    }
                );
                assert_eq!(
                    vault.read_optional(&plan.backup).unwrap(),
                    if undo {
                        original_backup
                    } else {
                        Some(b"old".to_vec())
                    }
                );
                assert_eq!(
                    vault.read_optional(&plan.work).unwrap(),
                    if undo {
                        original_work
                    } else {
                        Some(proposed.to_vec())
                    }
                );
                if kind == PlanKind::Publish && !undo {
                    assert_eq!(vault.read("published.md").unwrap(), b"old");
                    assert!(vault.read_optional("inbox/draft.md").unwrap().is_none());
                } else {
                    assert_eq!(
                        vault.read("inbox/draft.md").unwrap(),
                        if undo { b"old" } else { proposed }
                    );
                    assert!(vault.read_optional("published.md").unwrap().is_none());
                }
            }
        }
    }
}

#[test]
fn resume_retries_directory_confirmation_after_target_sync_failure() {
    for (kind, config_written) in [
        (PlanKind::Replace, false),
        (PlanKind::Edit, false),
        (PlanKind::Publish, false),
        (PlanKind::Init, false),
        (PlanKind::Init, true),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        let vault = if kind == PlanKind::Init {
            FAIL_AFTER.with(|p| p.set(Some("prepared")));
            assert_eq!(
                Vault::initialize(&root, "root.md", b"root", "config")
                    .unwrap_err()
                    .code,
                "interrupted"
            );
            FAIL_AFTER.with(|p| p.set(None));
            Vault::open(&root).unwrap()
        } else {
            Vault::initialize(&root, "root.md", b"root", "config").unwrap()
        };
        let guard = vault.lock().unwrap();
        if kind != PlanKind::Init {
            vault.create(&guard, "inbox/draft.md", b"old").unwrap();
        }
        let target = match kind {
            PlanKind::Init => "root.md",
            PlanKind::Publish => "published.md",
            _ => "inbox/draft.md",
        };
        let parent = vault.path(target).unwrap().parent().unwrap().to_path_buf();
        fail_sync(Some(&parent), usize::from(config_written));
        let result = match kind {
            PlanKind::Replace => vault.replace(&guard, target, &hash(b"old"), b"new"),
            PlanKind::Edit => {
                let plan = vault.begin_edit(&guard, target).unwrap();
                vault.finish_edit(&guard, &plan.operation_id, b"new")
            }
            PlanKind::Publish => vault.publish(&guard, "inbox/draft.md", target, &hash(b"old")),
            PlanKind::Init => {
                let plan = vault
                    .plans()
                    .unwrap()
                    .into_iter()
                    .find(|p| p.unfinished())
                    .unwrap();
                vault.recover(&guard, &plan.operation_id, RecoveryMode::Resume)
            }
        };
        fail_sync(None, 0);
        assert_eq!(result.unwrap_err().code, "io", "{kind:?}/{config_written}");
        assert!(vault.read_optional(target).unwrap().is_some());
        if config_written {
            assert_eq!(vault.read(".zettorc").unwrap(), b"config");
        }
        let plan = vault
            .plans()
            .unwrap()
            .into_iter()
            .find(|p| p.unfinished())
            .unwrap();
        fail_sync(Some(&parent), usize::from(config_written));
        let retry = vault.recover(&guard, &plan.operation_id, RecoveryMode::Resume);
        let hits = SYNC_HITS.with(|hits| hits.borrow().clone());
        fail_sync(None, 0);
        assert_eq!(retry.unwrap_err().code, "io", "{kind:?}/{config_written}");
        assert!(hits.contains(&parent));
        assert!(vault.plan(&plan.operation_id).unwrap().unfinished());
        if kind == PlanKind::Publish {
            assert_eq!(vault.read("inbox/draft.md").unwrap(), b"old");
        }
        for _ in 0..2 {
            assert_eq!(
                vault
                    .recover(&guard, &plan.operation_id, RecoveryMode::Resume)
                    .unwrap()
                    .state,
                PlanState::Complete
            );
        }
    }
}

#[test]
fn publish_resume_retries_confirmation_of_already_absent_source() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::initialize(&dir.path().join("vault"), "root.md", b"root", "config").unwrap();
    let guard = vault.lock().unwrap();
    vault.create(&guard, "inbox/draft.md", b"old").unwrap();
    let parent = vault.root().join("inbox");
    fail_sync(Some(&parent), 0);
    let result = vault.publish(&guard, "inbox/draft.md", "published.md", &hash(b"old"));
    fail_sync(None, 0);
    assert_eq!(result.unwrap_err().code, "io");
    assert!(vault.read_optional("inbox/draft.md").unwrap().is_none());
    let plan = vault
        .plans()
        .unwrap()
        .into_iter()
        .find(|p| p.unfinished())
        .unwrap();
    fail_sync(Some(&parent), 0);
    let retry = vault.recover(&guard, &plan.operation_id, RecoveryMode::Resume);
    let hits = SYNC_HITS.with(|hits| hits.borrow().clone());
    fail_sync(None, 0);
    assert_eq!(retry.unwrap_err().code, "io");
    assert!(hits.contains(&parent));
    assert!(vault.plan(&plan.operation_id).unwrap().unfinished());
    for _ in 0..2 {
        assert_eq!(
            vault
                .recover(&guard, &plan.operation_id, RecoveryMode::Resume)
                .unwrap()
                .state,
            PlanState::Complete
        );
    }
}

#[test]
fn restore_retries_confirmation_of_existing_source_and_absent_target() {
    for (kind, target_removed) in [
        (PlanKind::Replace, false),
        (PlanKind::Edit, false),
        (PlanKind::Publish, false),
        (PlanKind::Publish, true),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let vault =
            Vault::initialize(&dir.path().join("vault"), "root.md", b"root", "config").unwrap();
        let guard = vault.lock().unwrap();
        vault.create(&guard, "inbox/draft.md", b"old").unwrap();
        let plan = match kind {
            PlanKind::Replace => vault
                .replace(&guard, "inbox/draft.md", &hash(b"old"), b"new")
                .unwrap(),
            PlanKind::Edit => {
                let plan = vault.begin_edit(&guard, "inbox/draft.md").unwrap();
                vault
                    .finish_edit(&guard, &plan.operation_id, b"new")
                    .unwrap()
            }
            PlanKind::Publish => vault
                .publish(&guard, "inbox/draft.md", "published.md", &hash(b"old"))
                .unwrap(),
            PlanKind::Init => unreachable!(),
        };
        let parent = if target_removed {
            vault.root().to_path_buf()
        } else {
            vault.root().join("inbox")
        };
        fail_sync(Some(&parent), 0);
        let result = vault.recover(&guard, &plan.operation_id, RecoveryMode::Restore);
        fail_sync(None, 0);
        assert_eq!(result.unwrap_err().code, "io");
        assert_eq!(vault.read("inbox/draft.md").unwrap(), b"old");
        assert_eq!(
            vault.read_optional("published.md").unwrap().is_none(),
            kind != PlanKind::Publish || target_removed
        );
        fail_sync(Some(&parent), 0);
        let retry = vault.recover(&guard, &plan.operation_id, RecoveryMode::Resume);
        let hits = SYNC_HITS.with(|hits| hits.borrow().clone());
        fail_sync(None, 0);
        assert_eq!(retry.unwrap_err().code, "io", "{kind:?}/{target_removed}");
        assert!(hits.contains(&parent));
        assert!(vault.plan(&plan.operation_id).unwrap().unfinished());
        for _ in 0..2 {
            assert_eq!(
                vault
                    .recover(&guard, &plan.operation_id, RecoveryMode::Restore)
                    .unwrap()
                    .state,
                PlanState::Abandoned
            );
        }
    }
}

#[test]
fn init_retries_confirmation_of_vault_ancestors_in_leaf_to_root_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().canonicalize().unwrap().join("a/b/vault");
    let parent = path.parent().unwrap();
    fail_sync(Some(parent), 0);
    let first = Vault::initialize(&path, "root.md", b"root", "config");
    let hits = SYNC_HITS.with(|hits| hits.borrow().clone());
    fail_sync(None, 0);
    assert_eq!(first.unwrap_err().code, "io");
    assert!(hits.contains(&parent.to_path_buf()));
    let vault = Vault::open(&path).unwrap();
    let plan = vault
        .plans()
        .unwrap()
        .into_iter()
        .find(|p| p.unfinished())
        .unwrap();
    fail_sync(Some(parent), 0);
    let retry = Vault::initialize(&path, "other.md", b"other", "other");
    let hits = SYNC_HITS.with(|hits| hits.borrow().clone());
    fail_sync(None, 0);
    assert_eq!(retry.unwrap_err().code, "io");
    assert!(hits.contains(&parent.to_path_buf()));
    assert!(vault.plan(&plan.operation_id).unwrap().unfinished());
    let guard = vault.lock().unwrap();
    vault
        .recover(&guard, &plan.operation_id, RecoveryMode::Resume)
        .unwrap();
    let hits = SYNC_HITS.with(|hits| hits.borrow().clone());
    let ancestors = vault
        .root()
        .ancestors()
        .skip(1)
        .map(Path::to_path_buf)
        .collect::<Vec<_>>();
    assert!(
        hits.windows(ancestors.len())
            .any(|paths| paths == ancestors)
    );
    assert_eq!(vault.read("root.md").unwrap(), b"root");
    assert_eq!(vault.read(".zettorc").unwrap(), b"config");
    assert!(vault.read_optional("other.md").unwrap().is_none());
    assert_eq!(
        vault.plan(&plan.operation_id).unwrap().state,
        PlanState::Complete
    );
}

#[test]
fn interrupted_writes_resume_from_actual_files() {
    for kind in [PlanKind::Replace, PlanKind::Publish, PlanKind::Init] {
        let steps: &[&str] = match kind {
            PlanKind::Publish => &["plan_saved", "prepared", "target_written", "source_removed"],
            PlanKind::Init => &["plan_saved", "prepared", "target_written", "config_written"],
            _ => &["plan_saved", "prepared", "target_written"],
        };
        for &step in steps {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("vault");
            if kind == PlanKind::Init {
                FAIL_AFTER.with(|p| p.set(Some(step)));
                assert_eq!(
                    Vault::initialize(&root, "root.md", b"root", "config")
                        .unwrap_err()
                        .code,
                    "interrupted"
                );
                FAIL_AFTER.with(|p| p.set(None));
                let vault = Vault::initialize(&root, "other.md", b"other", "other").unwrap();
                assert_eq!(vault.read("root.md").unwrap(), b"root");
                assert_eq!(vault.read(".zettorc").unwrap(), b"config");
            } else {
                let vault = Vault::initialize(&root, "root.md", b"root", "config").unwrap();
                let guard = vault.lock().unwrap();
                vault.create(&guard, "inbox/draft.md", b"old").unwrap();
                FAIL_AFTER.with(|p| p.set(Some(step)));
                let result = if kind == PlanKind::Publish {
                    vault.publish(&guard, "inbox/draft.md", "published.md", &hash(b"old"))
                } else {
                    vault.replace(&guard, "inbox/draft.md", &hash(b"old"), b"new")
                };
                assert_eq!(result.unwrap_err().code, "interrupted");
                FAIL_AFTER.with(|p| p.set(None));
                let plan = vault
                    .plans()
                    .unwrap()
                    .into_iter()
                    .find(|p| p.kind == kind)
                    .unwrap();
                assert_eq!(vault.read(&plan.backup).unwrap(), b"old");
                vault
                    .recover(&guard, &plan.operation_id, RecoveryMode::Resume)
                    .unwrap();
                vault
                    .recover(&guard, &plan.operation_id, RecoveryMode::Resume)
                    .unwrap();
                let output = if kind == PlanKind::Publish {
                    "published.md"
                } else {
                    "inbox/draft.md"
                };
                assert_eq!(
                    vault.read(output).unwrap(),
                    if kind == PlanKind::Publish {
                        b"old"
                    } else {
                        b"new"
                    }
                );
            }
        }
    }
}

#[test]
fn publication_does_not_delete_externally_changed_source() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::initialize(&dir.path().join("vault"), "root.md", b"root", "config").unwrap();
    let guard = vault.lock().unwrap();
    vault.create(&guard, "inbox/draft.md", b"old").unwrap();
    FAIL_AFTER.with(|p| p.set(Some("target_written")));
    assert!(
        vault
            .publish(&guard, "inbox/draft.md", "published.md", &hash(b"old"))
            .is_err()
    );
    FAIL_AFTER.with(|p| p.set(None));
    fs::write(vault.path("inbox/draft.md").unwrap(), b"external").unwrap();
    let plan = vault
        .plans()
        .unwrap()
        .into_iter()
        .find(|p| p.kind == PlanKind::Publish)
        .unwrap();
    assert_eq!(
        vault
            .recover(&guard, &plan.operation_id, RecoveryMode::Resume)
            .unwrap_err()
            .code,
        "conflict"
    );
    assert_eq!(vault.read("inbox/draft.md").unwrap(), b"external");
    assert_eq!(vault.read("published.md").unwrap(), b"old");
}
