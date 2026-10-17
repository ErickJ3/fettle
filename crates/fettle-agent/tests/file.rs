use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::Path;

use fettle_agent::file::{File, State};
use fettle_agent::{AnyResource, Change, Ctx, Resource};
use fettle_protocol::Action;
use serde_json::json;
use tempfile::TempDir;

fn present(path: &Path, content: &str) -> File {
    File::new(
        path.to_str().unwrap(),
        &fettle_protocol::File::Present {
            content: content.to_owned(),
        },
    )
}

fn absent(path: &Path) -> File {
    File::new(path.to_str().unwrap(), &fettle_protocol::File::Absent)
}

fn state(content: &str) -> State {
    State {
        content: content.to_owned(),
    }
}

fn change(action: Action, detail: Option<&str>) -> Change {
    Change {
        action,
        detail: detail.map(str::to_owned),
    }
}

fn ctx() -> Ctx {
    Ctx::default()
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

#[test]
fn existing_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("motd");
    fs::write(&path, "hi\n").unwrap();

    assert_eq!(
        Resource::read(&present(&path, "hi\n"), &ctx()).unwrap(),
        Some(state("hi\n"))
    );
}

#[test]
fn missing_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("motd");

    assert_eq!(
        Resource::read(&present(&path, "hi\n"), &ctx()).unwrap(),
        None
    );
    assert!(!path.exists());
}

#[test]
fn directory_at_the_path() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("caddy");
    fs::create_dir(&path).unwrap();

    let error = Resource::read(&present(&path, "hi\n"), &ctx())
        .unwrap_err()
        .to_string();
    assert!(error.contains(path.to_str().unwrap()), "{error}");
}

#[test]
fn content_already_matches() {
    let file = present(Path::new("/etc/motd"), "hi\n");

    assert_eq!(Resource::diff(&file, Some(&state("hi\n"))), []);
}

#[test]
fn new_file() {
    let file = present(Path::new("/etc/motd"), "a\nb\nc\n");

    assert_eq!(
        Resource::diff(&file, None),
        [change(Action::Create, Some("+3 lines"))]
    );
}

#[test]
fn one_line_replaced_one_added() {
    let file = present(Path::new("/etc/motd"), "a\nc\nd\n");

    assert_eq!(
        Resource::diff(&file, Some(&state("a\nb\n"))),
        [change(Action::Update, Some("+2 \u{2212}1 lines"))]
    );
}

#[test]
fn line_moved() {
    let file = present(Path::new("/etc/motd"), "b\na\n");

    assert_eq!(
        Resource::diff(&file, Some(&state("a\nb\n"))),
        [change(Action::Update, Some("+1 \u{2212}1 lines"))]
    );
}

#[test]
fn absent_file_with_state_diffs_to_remove() {
    let file = absent(Path::new("/etc/motd"));

    assert_eq!(
        Resource::diff(&file, Some(&state("hi\n"))),
        [change(Action::Remove, None)]
    );
}

#[test]
fn already_absent() {
    let file = absent(Path::new("/etc/motd"));

    assert_eq!(Resource::diff(&file, None), []);
}

#[test]
fn non_utf8_file_diffs_to_an_update() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("motd");
    fs::write(&path, b"hi\xff\n").unwrap();
    let file: Box<dyn AnyResource> = Box::new(present(&path, "hi\n"));

    let have = file.read(&ctx()).unwrap();
    assert_eq!(have, Some(json!({ "content": "hi\u{fffd}\n" })));
    assert_eq!(
        file.diff(have.as_ref()).unwrap(),
        [change(Action::Update, Some("+1 \u{2212}1 lines"))]
    );
}

#[test]
fn create_and_converge() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("motd");
    let file: Box<dyn AnyResource> = Box::new(present(&path, "hi\n"));

    let have = file.read(&ctx()).unwrap();
    let changes = file.diff(have.as_ref()).unwrap();
    assert_eq!(changes, [change(Action::Create, Some("+1 lines"))]);
    file.apply(&ctx(), &changes).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "hi\n");

    let have = file.read(&ctx()).unwrap();
    assert_eq!(file.diff(have.as_ref()).unwrap(), []);
}

#[test]
fn new_file_is_world_readable() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("motd");
    let file = present(&path, "hi\n");

    Resource::apply(&file, &ctx(), &Resource::diff(&file, None)).unwrap();

    assert_eq!(mode(&path), 0o644);
}

#[test]
fn private_file_keeps_its_mode() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("secret");
    fs::write(&path, "old\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let before = fs::metadata(&path).unwrap();
    let file = present(&path, "new\n");

    Resource::apply(&file, &ctx(), &Resource::diff(&file, Some(&state("old\n")))).unwrap();

    let after = fs::metadata(&path).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "new\n");
    assert_eq!(mode(&path), 0o600);
    assert_eq!((after.uid(), after.gid()), (before.uid(), before.gid()));
}

/// Runs only as root, the one user that can create a file for another owner.
#[test]
fn foreign_owner_and_setuid_survive_an_update() {
    if fs::metadata("/proc/self").map(|m| m.uid()).ok() != Some(0) {
        return;
    }
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("tool");
    fs::write(&path, "old\n").unwrap();
    std::os::unix::fs::chown(&path, Some(1000), Some(1000)).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o4755)).unwrap();
    let file = present(&path, "new\n");

    Resource::apply(&file, &ctx(), &Resource::diff(&file, Some(&state("old\n")))).unwrap();

    let after = fs::metadata(&path).unwrap();
    assert_eq!((after.uid(), after.gid()), (1000, 1000));
    assert_eq!(mode(&path), 0o4755);
}

#[test]
fn write_through_a_link() {
    let dir = TempDir::new().unwrap();
    let link = dir.path().join("motd");
    let real = dir.path().join("run-motd");
    fs::write(&real, "old\n").unwrap();
    symlink(&real, &link).unwrap();
    let file = present(&link, "new\n");

    Resource::apply(&file, &ctx(), &Resource::diff(&file, Some(&state("old\n")))).unwrap();

    assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
    assert_eq!(fs::read_to_string(&real).unwrap(), "new\n");
}

#[test]
fn write_through_a_dangling_link() {
    let dir = TempDir::new().unwrap();
    let link = dir.path().join("motd");
    symlink("run-motd", &link).unwrap();
    let file = present(&link, "hi\n");

    assert_eq!(Resource::read(&file, &ctx()).unwrap(), None);
    Resource::apply(&file, &ctx(), &Resource::diff(&file, None)).unwrap();

    assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
    assert_eq!(
        fs::read_to_string(dir.path().join("run-motd")).unwrap(),
        "hi\n"
    );
}

#[test]
fn directory_not_created_yet() {
    let dir = TempDir::new().unwrap();
    let parent = dir.path().join("caddy");
    let file = present(&parent.join("Caddyfile"), "hi\n");

    let error = Resource::apply(&file, &ctx(), &Resource::diff(&file, None))
        .unwrap_err()
        .to_string();

    assert!(
        error.contains(&format!("directory {}", parent.display())),
        "{error}"
    );
    assert!(!parent.exists());
}

#[test]
fn delete_an_existing_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("motd");
    fs::write(&path, "hi\n").unwrap();
    let file: Box<dyn AnyResource> = Box::new(absent(&path));

    let have = file.read(&ctx()).unwrap();
    let changes = file.diff(have.as_ref()).unwrap();
    assert_eq!(changes, [change(Action::Remove, None)]);
    file.apply(&ctx(), &changes).unwrap();

    assert!(!path.exists());
}

#[test]
fn remove_a_file_fettle_created() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("motd");
    fs::write(&path, "hi\n").unwrap();
    let file: Box<dyn AnyResource> = Box::new(present(&path, "anything"));

    let recorded = file.read(&ctx()).unwrap().unwrap();
    file.remove(&ctx(), &recorded).unwrap();

    assert!(!path.exists());
}

#[test]
fn remove_a_file_already_gone() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("motd");

    Resource::remove(&present(&path, "anything"), &ctx(), &state("hi\n")).unwrap();
}

#[test]
fn removal_with_a_foreign_state() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("motd");
    fs::write(&path, "hi\n").unwrap();
    let file: Box<dyn AnyResource> = Box::new(present(&path, "hi\n"));

    assert!(file.remove(&ctx(), &json!({ "version": "1.0" })).is_err());
    assert!(path.exists());
}
