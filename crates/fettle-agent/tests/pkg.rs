use std::fs;
use std::os::unix::fs::MetadataExt;
use std::process::Command;
use std::sync::{Mutex, MutexGuard, PoisonError};

use fettle_agent::pkg::{Pkg, State};
use fettle_agent::{AnyResource, Change, Ctx, Resource};
use fettle_protocol::Action;
use tempfile::TempDir;

/// Not shipped by the CI runner image, small, and in Debian and Ubuntu main.
const MISSING: &str = "hello";
const LOCAL: &str = "fettle-test-conffile";

/// Host tests share one package database, so they run one at a time.
static APT: Mutex<()> = Mutex::new(());

fn pkg(name: &str) -> Pkg {
    Pkg::new(name, &fettle_protocol::Pkg { absent: false })
}

fn absent(name: &str) -> Pkg {
    Pkg::new(name, &fettle_protocol::Pkg { absent: true })
}

fn installed() -> State {
    State {
        version: "1.0".to_owned(),
    }
}

fn change(action: Action) -> Change {
    Change {
        action,
        detail: None,
    }
}

fn ctx() -> Ctx {
    Ctx::default()
}

#[test]
fn version_pin() {
    let error = Resource::read(&pkg("nginx=1.26.0-1"), &ctx())
        .unwrap_err()
        .to_string();

    assert_eq!(error, "pkg nginx=1.26.0-1: not a Debian package name");
}

#[test]
fn names_apt_would_misread() {
    for name in [
        "nginx/bookworm-backports",
        "-oAPT::x",
        "Nginx",
        "a",
        "libc6:",
        "_x",
    ] {
        let error = Resource::read(&pkg(name), &ctx()).unwrap_err().to_string();
        assert!(
            error.ends_with("not a Debian package name"),
            "{name}: {error}"
        );
    }
}

#[test]
fn architecture_qualifier() {
    // Off Debian the read fails running dpkg-query, past the name check.
    if let Err(error) = Resource::read(&pkg("libc6:amd64"), &ctx()) {
        assert!(error.to_string().contains("dpkg-query"), "{error}");
    }
}

#[test]
fn missing_package_diffs_to_create() {
    assert_eq!(Resource::diff(&pkg("sl"), None), [change(Action::Create)]);
}

#[test]
fn already_installed() {
    assert_eq!(Resource::diff(&pkg("htop"), Some(&installed())), []);
}

#[test]
fn absent_package_installed_diffs_to_remove() {
    assert_eq!(
        Resource::diff(&absent("snapd"), Some(&installed())),
        [change(Action::Remove)]
    );
}

#[test]
fn already_absent() {
    assert_eq!(Resource::diff(&absent("snapd"), None), []);
}

/// `None` unless `FETTLE_TEST_HOST` is set; set without root it panics, so a misconfigured CI
/// step fails instead of passing empty.
fn apt_host() -> Option<MutexGuard<'static, ()>> {
    std::env::var_os("FETTLE_TEST_HOST")?;
    let uid = fs::metadata("/proc/self").map(|m| m.uid());
    assert_eq!(uid.ok(), Some(0), "FETTLE_TEST_HOST needs root");
    Some(APT.lock().unwrap_or_else(PoisonError::into_inner))
}

fn sh(script: &str) {
    let status = Command::new("sh").args(["-c", script]).status().unwrap();
    assert!(status.success(), "{script}");
}

fn dpkg_status(name: &str) -> String {
    dpkg_query(name, "${db:Status-Abbrev}")
}

fn dpkg_query(name: &str, format: &str) -> String {
    let output = Command::new("dpkg-query")
        .args(["-W", "-f", format, name])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Installs, with dpkg rather than fettle, a local package that has a configuration file.
fn install_local_package() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("root");
    fs::create_dir_all(root.join("DEBIAN")).unwrap();
    fs::create_dir_all(root.join("etc")).unwrap();
    fs::write(
        root.join("DEBIAN/control"),
        format!(
            "Package: {LOCAL}\nVersion: 1.0\nArchitecture: all\nMaintainer: fettle <fettle@invalid>\nDescription: fettle test package with a configuration file\n"
        ),
    )
    .unwrap();
    fs::write(
        root.join("DEBIAN/conffiles"),
        format!("/etc/{LOCAL}.conf\n"),
    )
    .unwrap();
    fs::write(root.join(format!("etc/{LOCAL}.conf")), "setting = 1\n").unwrap();
    let deb = dir.path().join("local.deb");
    sh(&format!(
        "dpkg-deb --build {} {} && dpkg -i {}",
        root.display(),
        deb.display(),
        deb.display()
    ));
}

#[test]
fn package_not_installed() {
    let Some(_apt) = apt_host() else { return };

    assert_eq!(
        Resource::read(&pkg("fettle-never-installed"), &ctx()).unwrap(),
        None
    );
}

#[test]
fn install_converge_and_remove() {
    let Some(_apt) = apt_host() else { return };
    let ctx = ctx();
    let erased: Box<dyn AnyResource> = Box::new(pkg(MISSING));
    assert_eq!(
        erased.read(&ctx).unwrap(),
        None,
        "{MISSING} must start uninstalled"
    );

    let changes = erased.diff(None).unwrap();
    assert_eq!(changes, [change(Action::Create)]);
    erased.apply(&ctx, &changes).unwrap();
    let recorded = erased.read(&ctx).unwrap().expect("installed after apply");
    assert_eq!(recorded["version"], dpkg_query(MISSING, "${Version}"));
    assert_eq!(erased.diff(Some(&recorded)).unwrap(), []);

    erased.remove(&ctx, &recorded).unwrap();
    assert_eq!(erased.read(&ctx).unwrap(), None);

    erased.remove(&ctx, &recorded).unwrap();
    assert_eq!(erased.read(&ctx).unwrap(), None);
}

#[test]
fn uninstall_leaves_only_configuration_files() {
    let Some(_apt) = apt_host() else { return };
    let ctx = ctx();
    install_local_package();
    let erased: Box<dyn AnyResource> = Box::new(absent(LOCAL));

    let have = erased.read(&ctx).unwrap();
    assert!(have.is_some());
    let changes = erased.diff(have.as_ref()).unwrap();
    assert_eq!(changes, [change(Action::Remove)]);
    erased.apply(&ctx, &changes).unwrap();

    assert_eq!(dpkg_status(LOCAL), "rc ");
    assert_eq!(erased.read(&ctx).unwrap(), None);
    sh(&format!("dpkg --purge {LOCAL}"));
}

#[test]
fn unknown_package() {
    let Some(_apt) = apt_host() else { return };
    let name = "does-not-exist";

    let error = Resource::apply(&pkg(name), &ctx(), &[change(Action::Create)])
        .unwrap_err()
        .to_string();

    assert!(error.starts_with(&format!("pkg {name}: ")), "{error}");
    assert!(error.contains("Unable to locate package"), "{error}");
}
