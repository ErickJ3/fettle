use std::fmt::Write as _;
use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use fettle_agent::state::{HostState, Origin};
use fettle_protocol::{Kind, ResourceKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

fn key(kind: Kind, id: &str) -> ResourceKey {
    ResourceKey {
        kind,
        id: id.to_owned(),
    }
}

fn pkg(id: &str) -> ResourceKey {
    key(Kind::Pkg, id)
}

fn state_path(dir: &TempDir) -> PathBuf {
    dir.path().join("fettle").join("state.json")
}

fn open(path: &Path) -> HostState {
    HostState::open(path).unwrap()
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn sha256(state: &Value) -> String {
    let digest = Sha256::digest(serde_json::to_vec(state).unwrap());
    digest.iter().fold(String::from("sha256:"), |mut hex, b| {
        write!(hex, "{b:02x}").unwrap();
        hex
    })
}

#[test]
fn entry_written_for_a_package() {
    let dir = TempDir::new().unwrap();
    let path = state_path(&dir);
    let mut state = open(&path);
    let version = json!({"version": "3.3.0"});
    state.record(pkg("htop"), version.clone(), false);
    state.save().unwrap();

    let saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        saved,
        json!({"resources": [{
            "kind": "pkg",
            "id": "htop",
            "origin": "created",
            "hash": sha256(&version),
            "state": version,
        }]})
    );
}

#[test]
fn same_id_in_two_kinds() {
    let dir = TempDir::new().unwrap();
    let mut state = open(&state_path(&dir));
    state.record(pkg("caddy"), json!({"version": "2"}), false);
    state.record(key(Kind::Service, "caddy"), json!({"running": true}), false);

    assert_eq!(state.entries().len(), 2);
    assert_eq!(
        state.get(&pkg("caddy")).unwrap().state,
        json!({"version": "2"})
    );
    assert_eq!(
        state.get(&key(Kind::Service, "caddy")).unwrap().state,
        json!({"running": true})
    );
}

#[test]
fn package_fettle_installed() {
    let dir = TempDir::new().unwrap();
    let mut state = open(&state_path(&dir));
    state.record(pkg("htop"), json!({"version": "3.3.0"}), false);
    assert_eq!(state.get(&pkg("htop")).unwrap().origin, Origin::Created);
}

#[test]
fn package_already_installed() {
    let dir = TempDir::new().unwrap();
    let mut state = open(&state_path(&dir));
    state.record(pkg("htop"), json!({"version": "3.3.0"}), true);
    assert_eq!(state.get(&pkg("htop")).unwrap().origin, Origin::Preexisting);
}

#[test]
fn origin_survives_later_writes() {
    let dir = TempDir::new().unwrap();
    let motd = key(Kind::File, "/etc/motd");
    let mut state = open(&state_path(&dir));
    state.record(motd.clone(), json!({"content": "old\n"}), false);
    let new = json!({"content": "new\n"});
    state.record(motd.clone(), new.clone(), true);

    let entry = state.get(&motd).unwrap();
    assert_eq!(entry.origin, Origin::Created);
    assert_eq!(entry.state, new);
    assert_eq!(entry.hash, sha256(&new));
    assert_eq!(state.entries().len(), 1);
}

#[test]
fn created_by_an_earlier_write_in_the_same_run() {
    let dir = TempDir::new().unwrap();
    let caddyfile = key(Kind::File, "/etc/caddy/Caddyfile");
    let mut state = open(&state_path(&dir));
    let found_at_plan = false;
    state.record(pkg("caddy"), json!({"version": "2"}), false);
    state.record(
        caddyfile.clone(),
        json!({"content": ":80\n"}),
        found_at_plan,
    );
    assert_eq!(state.get(&caddyfile).unwrap().origin, Origin::Created);
}

#[test]
fn absent_package_removed() {
    let dir = TempDir::new().unwrap();
    let path = state_path(&dir);
    let mut state = open(&path);
    state.discard(&pkg("snapd"));
    state.save().unwrap();
    drop(state);

    assert!(open(&path).get(&pkg("snapd")).is_none());
}

#[test]
fn absent_declaration_for_a_resource_fettle_created() {
    let dir = TempDir::new().unwrap();
    let path = state_path(&dir);
    let mut state = open(&path);
    state.record(pkg("nginx"), json!({"version": "1.26"}), false);
    state.save().unwrap();
    drop(state);

    let mut state = open(&path);
    assert_eq!(state.get(&pkg("nginx")).unwrap().origin, Origin::Created);
    state.discard(&pkg("nginx"));
    state.save().unwrap();
    drop(state);

    assert!(open(&path).get(&pkg("nginx")).is_none());
}

#[test]
fn forgotten_resource() {
    let dir = TempDir::new().unwrap();
    let path = state_path(&dir);
    let mut state = open(&path);
    state.record(pkg("curl"), json!({"version": "8"}), false);
    state.record(pkg("htop"), json!({"version": "3.3.0"}), true);
    state.record(
        key(Kind::File, "/etc/motd"),
        json!({"content": "hi\n"}),
        false,
    );
    let others: Vec<_> = state
        .entries()
        .iter()
        .filter(|entry| entry.key != pkg("htop"))
        .cloned()
        .collect();
    state.discard(&pkg("htop"));
    state.save().unwrap();
    drop(state);

    assert_eq!(open(&path).entries(), others);
}

#[test]
fn saved_state_read_back() {
    let dir = TempDir::new().unwrap();
    let path = state_path(&dir);
    let mut state = open(&path);
    state.record(pkg("htop"), json!({"version": "3.3.0"}), true);
    state.record(
        key(Kind::File, "/etc/motd"),
        json!({"content": "hi\n"}),
        false,
    );
    state.save().unwrap();
    let saved = state.entries().to_vec();
    drop(state);

    assert_eq!(open(&path).entries(), saved);
}

#[test]
fn first_run_on_a_host() {
    let dir = TempDir::new().unwrap();
    let path = state_path(&dir);
    let state = open(&path);
    assert_eq!(mode(path.parent().unwrap()), 0o700);
    state.save().unwrap();
    assert_eq!(mode(&path), 0o600);
}

#[test]
fn existing_directory_keeps_its_mode() {
    let dir = TempDir::new().unwrap();
    let path = state_path(&dir);
    fs::create_dir(path.parent().unwrap()).unwrap();
    fs::set_permissions(path.parent().unwrap(), Permissions::from_mode(0o750)).unwrap();
    drop(open(&path));
    assert_eq!(mode(path.parent().unwrap()), 0o750);
}

#[test]
fn no_state_yet() {
    let dir = TempDir::new().unwrap();
    assert_eq!(open(&state_path(&dir)).entries(), []);
}

#[test]
fn corrupt_file() {
    let dir = TempDir::new().unwrap();
    let path = state_path(&dir);
    fs::create_dir(path.parent().unwrap()).unwrap();
    fs::write(&path, "{\"resources\": [").unwrap();

    let error = HostState::open(&path).unwrap_err().to_string();
    assert!(error.contains(path.to_str().unwrap()), "{error}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "{\"resources\": [");
}

#[test]
fn duplicate_key() {
    let dir = TempDir::new().unwrap();
    let path = state_path(&dir);
    fs::create_dir(path.parent().unwrap()).unwrap();
    let entry =
        json!({"kind": "pkg", "id": "htop", "origin": "created", "hash": "sha256:00", "state": {}});
    let content = json!({"resources": [entry, entry]}).to_string();
    fs::write(&path, &content).unwrap();

    let error = HostState::open(&path).unwrap_err().to_string();
    assert!(error.contains(path.to_str().unwrap()), "{error}");
    assert!(error.contains("pkg htop"), "{error}");
    assert_eq!(fs::read_to_string(&path).unwrap(), content);
}

#[test]
fn writes_before_a_failure() {
    let dir = TempDir::new().unwrap();
    let path = state_path(&dir);
    let mut state = open(&path);
    state.record(pkg("htop"), json!({"version": "3.3.0"}), false);
    state.save().unwrap();
    drop(state);

    assert!(open(&path).get(&pkg("htop")).is_some());
}

#[test]
fn failed_save_keeps_the_previous_file() {
    let dir = TempDir::new().unwrap();
    let path = state_path(&dir);
    let state_dir = path.parent().unwrap();
    let mut state = open(&path);
    state.record(pkg("htop"), json!({"version": "3.3.0"}), false);
    state.save().unwrap();
    let previous = fs::read(&path).unwrap();

    fs::set_permissions(state_dir, Permissions::from_mode(0o500)).unwrap();
    if fs::write(state_dir.join("probe"), "").is_ok() {
        return;
    }
    state.record(pkg("curl"), json!({"version": "8"}), false);
    let saved = state.save();
    fs::set_permissions(state_dir, Permissions::from_mode(0o700)).unwrap();

    assert!(saved.is_err());
    assert_eq!(fs::read(&path).unwrap(), previous);
    let mut names: Vec<_> = fs::read_dir(state_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    names.sort();
    assert_eq!(names, ["state.json", "state.lock"]);
}

#[test]
fn concurrent_run() {
    let dir = TempDir::new().unwrap();
    let path = state_path(&dir);
    let mut first = open(&path);

    let error = HostState::open(&path).unwrap_err().to_string();
    assert!(error.contains("state.lock"), "{error}");
    assert!(error.contains("another fettle run"), "{error}");

    first.record(pkg("htop"), json!({"version": "3.3.0"}), false);
    first.save().unwrap();
    assert!(fs::read_to_string(&path).unwrap().contains("\"htop\""));
}

#[test]
fn lock_released_at_the_end_of_a_run() {
    let dir = TempDir::new().unwrap();
    let path = state_path(&dir);
    drop(open(&path));
    drop(HostState::open(&path).unwrap());
}
