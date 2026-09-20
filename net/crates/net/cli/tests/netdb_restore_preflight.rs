//! Restore preflight must preserve offline stores before any application begins.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use net_sdk::cortex::NetDbSnapshot;
use serde_json::Value;

struct Fixture {
    root: tempfile::TempDir,
    config: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        private_config(&config, "");
        Self { root, config }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("net-mesh"));
        // Do not inherit a developer's profile or permission override.
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("NET_MESH_") {
                cmd.env_remove(name);
            }
        }
        cmd.current_dir(self.root.path())
            .env("HOME", self.root.path())
            .env("XDG_DATA_HOME", self.path("data"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .args(["--config"])
            .arg(&self.config)
            .args(["--profile", "default", "--output", "json", "netdb"]);
        cmd
    }

    fn task(&self, store: &Path, id: &str, origin: &str) {
        success(
            self.command()
                .args([
                    "tasks", "create", id, "--title", id, "--origin", origin, "--store",
                ])
                .arg(store)
                .output()
                .unwrap(),
        );
    }

    fn snapshot(&self, store: &Path, target: &Path, tasks: bool, origin: &str) {
        success(
            self.command()
                .args(["snapshot", "--store"])
                .arg(store)
                .arg("--out")
                .arg(target)
                .args([
                    "--origin",
                    origin,
                    "--with-tasks",
                    if tasks { "true" } else { "false" },
                    "--with-memories",
                    if tasks { "false" } else { "true" },
                ])
                .output()
                .unwrap(),
        );
    }

    fn restore(&self, store: &Path, source: &Path) -> Command {
        let mut cmd = self.command();
        cmd.args(["restore", "--store"])
            .arg(store)
            .arg("--from")
            .arg(source);
        cmd
    }

    fn ids(&self, store: &Path, family: &str, origin: &str) -> Vec<u64> {
        let value = success(
            self.command()
                .args([family, "ls", "--origin", origin, "--store"])
                .arg(store)
                .output()
                .unwrap(),
        );
        let mut ids: Vec<_> = value
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_u64().unwrap())
            .collect();
        ids.sort_unstable();
        ids
    }

    fn source(&self) -> PathBuf {
        let store = self.path("source");
        self.task(&store, "2", "17");
        assert_eq!(self.ids(&store, "tasks", "17"), [2]);
        let snapshot = self.path("snapshot.bin");
        self.snapshot(&store, &snapshot, true, "17");
        snapshot
    }
}

#[tokio::test]
async fn snapshot_fixture_restores_record_in_memory() {
    use net_sdk::cortex::{NetDb, Redex};

    let f = Fixture::new();
    let source = f.source();
    let snapshot = NetDbSnapshot::decode(&fs::read(source).unwrap()).unwrap();
    let db = NetDb::builder(Redex::new())
        .origin(17)
        .with_tasks()
        .build_from_snapshot(&snapshot)
        .await
        .unwrap();
    let state = db.try_tasks().unwrap().state();
    let ids: Vec<_> = state.read().all().map(|task| task.id).collect();
    assert_eq!(ids, [2]);
    db.close().unwrap();
}

fn private_config(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn success(out: Output) -> Value {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

fn failure(out: Output, diagnostic: &str) {
    assert!(
        !out.status.success(),
        "unexpected success: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        out.stdout.is_empty(),
        "failure emitted a result: {:?}",
        out.stdout
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains(diagnostic),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// Include directory entries as well as file bytes; compare BEFORE reopening
// the store, since logical verification may itself update persistent files.
fn inventory(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn walk(root: &Path, path: &Path, result: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                result.insert(path.strip_prefix(root).unwrap().to_owned(), None);
                walk(root, &path, result);
            } else {
                result.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    Some(fs::read(&path).unwrap()),
                );
            }
        }
    }
    let mut result = BTreeMap::new();
    walk(root, root, &mut result);
    result
}

fn rejects_source(make_source: impl FnOnce(&Path), diagnostic: &str) {
    let f = Fixture::new();
    let dest = f.path("destination");
    f.task(&dest, "1", "17");
    let before = inventory(&dest);
    let source = f.path("invalid.bin");
    make_source(&source);
    failure(
        f.restore(&dest, &source)
            .args(["--origin", "17", "--clear"])
            .output()
            .unwrap(),
        diagnostic,
    );
    assert_eq!(inventory(&dest), before);
    assert_eq!(f.ids(&dest, "tasks", "17"), [1]);
}

#[test]
fn missing_snapshot_does_not_clear_existing_store() {
    rejects_source(|_| {}, "snapshot file");
}

#[test]
fn malformed_snapshot_does_not_clear_existing_store() {
    rejects_source(|p| fs::write(p, [255]).unwrap(), "snapshot decode");
}

#[test]
fn unreadable_snapshot_does_not_clear_existing_store() {
    rejects_source(|p| fs::create_dir(p).unwrap(), "snapshot file");
}

#[test]
fn oversized_snapshot_does_not_clear_existing_store() {
    rejects_source(
        |p| {
            let file = fs::File::create(p).unwrap();
            // NTFS needs an explicit sparse flag before extending a file; Unix
            // set_len creates a hole. Never allocate a multi-gigabyte fixture.
            #[cfg(windows)]
            assert!(Command::new("fsutil")
                .args(["sparse", "setflag"])
                .arg(p)
                .status()
                .unwrap()
                .success());
            file.set_len(4 * 1024 * 1024 * 1024 + 1).unwrap();
        },
        "ceiling",
    );
}

#[test]
fn snapshot_without_adapters_does_not_clear_existing_store() {
    rejects_source(
        |p| {
            fs::write(
                p,
                NetDbSnapshot {
                    tasks: None,
                    memories: None,
                }
                .encode()
                .unwrap(),
            )
            .unwrap()
        },
        "neither tasks nor memories",
    );
}

#[test]
fn invalid_snapshot_does_not_create_absent_destination() {
    let f = Fixture::new();
    let source = f.path("empty.bin");
    fs::write(
        &source,
        NetDbSnapshot {
            tasks: None,
            memories: None,
        }
        .encode()
        .unwrap(),
    )
    .unwrap();
    let dest = f.path("absent");
    failure(
        f.restore(&dest, &source)
            .args(["--origin", "17", "--clear"])
            .output()
            .unwrap(),
        "neither tasks nor memories",
    );
    assert!(!dest.exists());
}

#[test]
fn invalid_profile_does_not_fall_back_or_mutate_store() {
    let f = Fixture::new();
    let dest = f.path("destination");
    let profile_store = f.path("profile-store");
    f.task(&dest, "1", "17");
    f.task(&profile_store, "3", "17");
    let source = f.source();
    let before = inventory(&dest);
    let profile_before = inventory(&profile_store);
    private_config(
        &f.config,
        &format!(
            "[default]\nnetdb = '{}'\ninvalid = [",
            profile_store.display()
        ),
    );
    failure(
        f.command()
            .args(["tasks", "create", "9", "--title", "unwanted", "--store"])
            .arg(&dest)
            .output()
            .unwrap(),
        "config load",
    );
    failure(
        f.restore(&dest, &source)
            .args(["--origin", "17", "--clear"])
            .output()
            .unwrap(),
        "config load",
    );
    assert_eq!(inventory(&dest), before);
    assert_eq!(inventory(&profile_store), profile_before);
    private_config(&f.config, "");
    assert_eq!(f.ids(&dest, "tasks", "17"), [1]);
    assert_eq!(f.ids(&profile_store, "tasks", "17"), [3]);
}

// dirs uses Windows known folders, NOT APPDATA/HOME overrides. Do not run
// an inverse mutation with an implicit Windows store: it could touch the
// operator's real data. Linux's XDG path is isolated in each child instead.
#[cfg(target_os = "linux")]
#[test]
fn invalid_profile_preserves_isolated_default_store() {
    let f = Fixture::new();
    let dest = f.path("data/net-mesh/netdb");
    let configured = f.path("configured-store");
    f.task(&dest, "1", "17");
    f.task(&configured, "3", "17");
    let source = f.source();
    let before = inventory(&dest);
    let configured_before = inventory(&configured);
    private_config(
        &f.config,
        &format!("[default]\nnetdb = '{}'\ninvalid = [", configured.display()),
    );
    failure(
        f.command()
            .args(["tasks", "create", "9", "--title", "unwanted"])
            .output()
            .unwrap(),
        "config load",
    );
    failure(
        f.command()
            .args(["restore", "--origin", "17", "--clear", "--from"])
            .arg(source)
            .output()
            .unwrap(),
        "config load",
    );
    assert_eq!(inventory(&dest), before);
    assert_eq!(inventory(&configured), configured_before);
    private_config(&f.config, "");
    assert_eq!(f.ids(&dest, "tasks", "17"), [1]);
}

#[test]
fn unreadable_config_does_not_mutate_store() {
    let f = Fixture::new();
    let dest = f.path("destination");
    f.task(&dest, "1", "17");
    let source = f.source();
    let before = inventory(&dest);
    fs::remove_file(&f.config).unwrap();
    fs::create_dir(&f.config).unwrap();
    failure(
        f.command()
            .args(["tasks", "create", "9", "--title", "unwanted", "--store"])
            .arg(&dest)
            .output()
            .unwrap(),
        "config load",
    );
    failure(
        f.restore(&dest, &source)
            .args(["--origin", "17", "--clear"])
            .output()
            .unwrap(),
        "config load",
    );
    assert_eq!(inventory(&dest), before);
    fs::remove_dir(&f.config).unwrap();
    private_config(&f.config, "");
    assert_eq!(f.ids(&dest, "tasks", "17"), [1]);
}

#[cfg(unix)]
#[test]
fn insecure_profile_is_rejected_before_mutation() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let dest = f.path("destination");
    f.task(&dest, "1", "17");
    let source = f.source();
    let before = inventory(&dest);
    // The secret-file gate rejects 0644 even under root, unlike chmod 000.
    fs::set_permissions(&f.config, fs::Permissions::from_mode(0o644)).unwrap();
    failure(
        f.command()
            .args(["tasks", "create", "9", "--title", "unwanted", "--store"])
            .arg(&dest)
            .output()
            .unwrap(),
        "config load",
    );
    failure(
        f.restore(&dest, &source)
            .args(["--origin", "17", "--clear"])
            .output()
            .unwrap(),
        "config load",
    );
    assert_eq!(inventory(&dest), before);
    private_config(&f.config, "");
    assert_eq!(f.ids(&dest, "tasks", "17"), [1]);
}

#[test]
fn valid_clear_restores_snapshot_after_preflight() {
    let f = Fixture::new();
    let source = f.source();
    let dest = f.path("destination");
    f.task(&dest, "1", "17");
    let result = success(
        f.restore(&dest, &source)
            .args(["--origin", "17", "--clear"])
            .output()
            .unwrap(),
    );
    assert_eq!(result["bytes_read"], fs::metadata(source).unwrap().len());
    assert_eq!(f.ids(&dest, "tasks", "17"), [2]);
}

#[test]
fn snapshot_inside_destination_is_loaded_before_clear() {
    let f = Fixture::new();
    let source = f.source();
    let dest = f.path("destination");
    f.task(&dest, "1", "17");
    let inside = dest.join("snapshot.bin");
    fs::copy(source, &inside).unwrap();
    success(
        f.restore(&dest, &inside)
            .args(["--origin", "17", "--clear"])
            .output()
            .unwrap(),
    );
    assert!(!inside.exists());
    assert_eq!(f.ids(&dest, "tasks", "17"), [2]);
}

#[test]
fn force_without_clear_preserves_merge_semantics() {
    let f = Fixture::new();
    let source = f.source();
    let dest = f.path("destination");
    f.task(&dest, "1", "17");
    // A tasks-only snapshot must not discard the existing memory chain.
    success(
        f.command()
            .args([
                "memories",
                "store",
                "3",
                "--content",
                "keep",
                "--origin",
                "17",
                "--store",
            ])
            .arg(&dest)
            .output()
            .unwrap(),
    );
    let before = inventory(&dest);
    failure(
        f.restore(&dest, &source)
            .args(["--origin", "17"])
            .output()
            .unwrap(),
        "already contains data",
    );
    assert_eq!(inventory(&dest), before);
    success(
        f.restore(&dest, &source)
            .args(["--origin", "17", "--force"])
            .output()
            .unwrap(),
    );
    assert_eq!(f.ids(&dest, "memories", "17"), [3]);
}

#[test]
fn valid_profile_store_is_honored() {
    let f = Fixture::new();
    let source = f.source();
    let dest = f.path("profile-store");
    private_config(
        &f.config,
        &format!("[default]\nnetdb = '{}'\n", dest.display()),
    );
    success(
        f.command()
            .args(["restore", "--origin", "17", "--from"])
            .arg(source)
            .output()
            .unwrap(),
    );
    assert_eq!(f.ids(&dest, "tasks", "17"), [2]);
}

#[test]
fn missing_optional_config_keeps_explicit_store_usable() {
    let f = Fixture::new();
    fs::remove_file(&f.config).unwrap();
    let dest = f.path("destination");
    f.task(&dest, "1", "17");
    assert_eq!(f.ids(&dest, "tasks", "17"), [1]);
}

#[test]
fn destination_inspection_failure_is_not_treated_as_empty() {
    let f = Fixture::new();
    let source = f.source();
    let dest = f.path("not-a-directory");
    fs::write(&dest, b"preserve").unwrap();
    let out = f
        .restore(&dest, &source)
        .args(["--origin", "17"])
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&out.stderr).contains("--force"));
    failure(out, "inspection");
    assert_eq!(fs::read(&dest).unwrap(), b"preserve");
}

#[test]
fn tasks_only_and_origin_zero_controls() {
    single_adapter_and_origin_zero_control(true);
}

#[test]
fn memories_only_and_origin_zero_controls() {
    single_adapter_and_origin_zero_control(false);
}

fn single_adapter_and_origin_zero_control(tasks: bool) {
    let f = Fixture::new();
    let store = f.path("source");
    if tasks {
        f.task(&store, "1", "0");
    } else {
        success(
            f.command()
                .args(["memories", "store", "1", "--content", "memory", "--store"])
                .arg(&store)
                .output()
                .unwrap(),
        );
    }
    let source = f.path("snapshot.bin");
    f.snapshot(&store, &source, tasks, "0");
    let dest = f.path("destination");
    failure(f.restore(&dest, &source).output().unwrap(), "--origin");
    failure(
        f.restore(&dest, &source)
            .args(["--origin", "0"])
            .output()
            .unwrap(),
        "--allow-origin-zero",
    );
    assert!(!dest.exists());
    success(
        f.restore(&dest, &source)
            .arg("--allow-origin-zero")
            .output()
            .unwrap(),
    );
    // Inspect paths without opening the absent adapter (which creates it).
    assert!(dest
        .join(if tasks {
            "cortex/tasks"
        } else {
            "cortex/memories"
        })
        .is_dir());
    assert!(!dest
        .join(if tasks {
            "cortex/memories"
        } else {
            "cortex/tasks"
        })
        .exists());
    assert_eq!(
        f.ids(&dest, if tasks { "tasks" } else { "memories" }, "0"),
        [1]
    );
}

#[test]
fn invalid_embedded_snapshot_does_not_clear_existing_store() {
    for tasks in [true, false] {
        rejects_source(
            |path| {
                let mut snapshot = NetDbSnapshot {
                    tasks: None,
                    memories: None,
                };
                let adapter = if tasks {
                    &mut snapshot.tasks
                } else {
                    &mut snapshot.memories
                };
                *adapter = Some((vec![255], Some(0)));
                fs::write(path, snapshot.encode().unwrap()).unwrap();
            },
            "snapshot validation",
        );
    }
}

#[test]
fn restored_records_and_subsequent_writes_survive_repeated_processes() {
    let f = Fixture::new();
    let source = f.source();
    let dest = f.path("destination");
    success(
        f.restore(&dest, &source)
            .args(["--origin", "17"])
            .output()
            .unwrap(),
    );
    fs::remove_file(source).unwrap();
    f.task(&dest, "3", "17");
    assert_eq!(f.ids(&dest, "tasks", "17"), [2, 3]);
    success(
        f.command()
            .args([
                "tasks", "rename", "2", "--title", "changed", "--origin", "17", "--store",
            ])
            .arg(&dest)
            .output()
            .unwrap(),
    );
    success(
        f.command()
            .args(["tasks", "delete", "3", "--origin", "17", "--store"])
            .arg(&dest)
            .output()
            .unwrap(),
    );
    let records = success(
        f.command()
            .args(["tasks", "ls", "--origin", "17", "--store"])
            .arg(&dest)
            .output()
            .unwrap(),
    );
    assert_eq!(records.as_array().unwrap().len(), 1);
    assert_eq!(records[0]["title"], "changed");
    failure(
        f.command()
            .args(["tasks", "ls", "--origin", "18", "--store"])
            .arg(&dest)
            .output()
            .unwrap(),
        "origin mismatch",
    );
    assert_eq!(f.ids(&dest, "tasks", "17"), [2]);
}

#[test]
fn checkpoint_input_failure_has_no_restore_success() {
    let f = Fixture::new();
    let source = f.source();
    let dest = f.path("destination");
    f.task(&dest, "1", "17");
    // A directory cannot be decoded or replaced as a checkpoint file.
    fs::create_dir(dest.join("cortex/tasks/cortex.snapshot")).unwrap();
    let before = inventory(&dest);
    failure(
        f.restore(&dest, &source)
            .args(["--origin", "17", "--force"])
            .output()
            .unwrap(),
        "netdb restore",
    );
    assert_eq!(inventory(&dest), before);
}

#[test]
fn corrupt_checkpoint_is_not_silently_ignored() {
    let f = Fixture::new();
    let source = f.source();
    let dest = f.path("destination");
    success(
        f.restore(&dest, &source)
            .args(["--origin", "17"])
            .output()
            .unwrap(),
    );
    fs::write(dest.join("cortex/tasks/cortex.snapshot"), [255]).unwrap();
    failure(
        f.command()
            .args(["tasks", "ls", "--origin", "17", "--store"])
            .arg(&dest)
            .output()
            .unwrap(),
        "checkpoint",
    );
}

#[cfg(windows)]
#[test]
fn checkpoint_replacement_failure_has_no_success_and_keeps_previous_checkpoint() {
    let f = Fixture::new();
    let source = f.source();
    let dest = f.path("destination");
    success(
        f.restore(&dest, &source)
            .args(["--origin", "17"])
            .output()
            .unwrap(),
    );
    let checkpoint = dest.join("cortex/tasks/cortex.snapshot");
    let before = fs::read(&checkpoint).unwrap();
    let original_permissions = fs::metadata(&checkpoint).unwrap().permissions();
    let mut readonly = original_permissions.clone();
    readonly.set_readonly(true);
    fs::set_permissions(&checkpoint, readonly).unwrap();
    let out = f
        .restore(&dest, &source)
        .args(["--origin", "17", "--force"])
        .output()
        .unwrap();
    // Restore the attribute even if the assertion below fails, so TempDir
    // can remove this disposable store. Windows rejects replacing readonly
    // files; unlike denying reads, this reaches the publication operation.
    fs::set_permissions(&checkpoint, original_permissions).unwrap();
    failure(out, "netdb restore");
    assert_eq!(fs::read(checkpoint).unwrap(), before);
    assert_eq!(f.ids(&dest, "tasks", "17"), [2]);
}
