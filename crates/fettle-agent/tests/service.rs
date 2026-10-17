use std::fs;
use std::os::unix::fs::MetadataExt;
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError};

use fettle_agent::service::{Service, State};
use fettle_agent::{Change, Ctx, Resource};
use fettle_protocol::Action;

/// A long-running unit that can be enabled.
const SLEEPER: &str =
    "[Service]\nExecStart=/bin/sleep infinity\n\n[Install]\nWantedBy=multi-user.target\n";

/// `daemon-reload` reloads every unit, so host tests run one at a time.
static SYSTEMD: Mutex<()> = Mutex::new(());

fn service(id: &str, enabled: Option<bool>) -> Service {
    Service::new(id, &fettle_protocol::Service { enabled })
}

fn state(enabled: bool, running: bool) -> State {
    State { enabled, running }
}

fn change(action: Action, detail: &str) -> Change {
    Change {
        action,
        detail: Some(detail.to_owned()),
    }
}

fn ctx() -> Ctx {
    Ctx::default()
}

#[test]
fn unit_installed_later_in_the_run() {
    assert_eq!(
        service("caddy", Some(true)).diff(None),
        [change(Action::Create, "enable, start")]
    );
}

/// Covers "Crashed service" too: a failed unit reads as enabled and not running.
#[test]
fn enabled_but_stopped() {
    assert_eq!(
        service("caddy", Some(true)).diff(Some(&state(true, false))),
        [change(Action::Update, "start")]
    );
}

#[test]
fn already_enabled_and_running() {
    assert_eq!(
        service("caddy", Some(true)).diff(Some(&state(true, true))),
        []
    );
}

#[test]
fn start_a_stopped_unit() {
    assert_eq!(
        service("caddy", Some(true)).diff(Some(&state(false, false))),
        [change(Action::Update, "enable, start")]
    );
}

#[test]
fn unit_missing() {
    assert_eq!(service("apache2", Some(false)).diff(None), []);
}

#[test]
fn already_disabled_and_stopped() {
    assert_eq!(
        service("apache2", Some(false)).diff(Some(&state(false, false))),
        []
    );
}

#[test]
fn declared_for_reference_only() {
    let caddy = service("caddy", None);
    assert_eq!(caddy.diff(Some(&state(false, false))), []);
    assert_eq!(caddy.diff(None), []);
}

#[test]
fn disable_a_running_unit() {
    assert_eq!(
        service("apache2", Some(false)).diff(Some(&state(true, true))),
        [change(Action::Update, "disable, stop")]
    );
}

#[test]
fn properties_parse_by_key() {
    let shown = "ActiveState=reloading\nUnitFileState=enabled\nLoadState=loaded\n";
    assert_eq!(State::from_systemctl_show(shown), Some(state(true, true)));

    let failed = "UnitFileState=enabled\nActiveState=failed\nLoadState=loaded\n";
    assert_eq!(State::from_systemctl_show(failed), Some(state(true, false)));

    let masked = "LoadState=masked\nUnitFileState=masked\nActiveState=inactive\n";
    assert_eq!(
        State::from_systemctl_show(masked),
        Some(state(false, false))
    );
}

#[test]
fn unit_systemd_cannot_find() {
    let shown = "LoadState=not-found\nUnitFileState=\nActiveState=inactive\n";
    assert_eq!(State::from_systemctl_show(shown), None);
}

/// `None` unless `FETTLE_TEST_HOST` is set; set without root it panics, so a misconfigured CI
/// step fails instead of passing empty.
fn systemd_host() -> Option<MutexGuard<'static, ()>> {
    std::env::var_os("FETTLE_TEST_HOST")?;
    let uid = fs::metadata("/proc/self").map(|m| m.uid());
    assert_eq!(uid.ok(), Some(0), "FETTLE_TEST_HOST needs root");
    Some(SYSTEMD.lock().unwrap_or_else(PoisonError::into_inner))
}

fn sh(script: &str) {
    let status = Command::new("sh").args(["-c", script]).status().unwrap();
    assert!(status.success(), "{script}");
}

/// A unit under `/etc/systemd/system`, stopped and deleted when dropped.
struct TestUnit(&'static str);

impl TestUnit {
    fn install(name: &'static str, body: &str) -> Self {
        fs::write(format!("/etc/systemd/system/{name}.service"), body).unwrap();
        sh("systemctl daemon-reload");
        Self(name)
    }
}

impl Drop for TestUnit {
    fn drop(&mut self) {
        let name = self.0;
        let _ = Command::new("sh")
            .args([
                "-c",
                &format!(
                    "systemctl disable --now {name}; systemctl unmask {name}; rm -f /etc/systemd/system/{name}.service; systemctl daemon-reload; systemctl reset-failed {name}"
                ),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn read(id: &str) -> Option<State> {
    service(id, None).read(&ctx()).unwrap()
}

#[test]
fn enabled_and_running_unit() {
    let Some(_systemd) = systemd_host() else {
        return;
    };
    let unit = TestUnit::install("fettle-test-running", SLEEPER);
    sh(&format!("systemctl enable --now {}", unit.0));

    assert_eq!(read(unit.0), Some(state(true, true)));
}

#[test]
fn unit_that_does_not_exist() {
    let Some(_systemd) = systemd_host() else {
        return;
    };

    assert_eq!(read("fettle-test-never-installed"), None);
}

#[test]
fn failed_unit() {
    let Some(_systemd) = systemd_host() else {
        return;
    };
    let unit = TestUnit::install(
        "fettle-test-failed",
        "[Service]\nType=oneshot\nExecStart=/bin/false\n\n[Install]\nWantedBy=multi-user.target\n",
    );
    sh(&format!(
        "systemctl enable {0}; ! systemctl start {0}",
        unit.0
    ));

    assert_eq!(read(unit.0), Some(state(true, false)));
}

#[test]
fn start_and_converge() {
    let Some(_systemd) = systemd_host() else {
        return;
    };
    let unit = TestUnit::install("fettle-test-start", SLEEPER);
    let caddy = service(unit.0, Some(true));
    let ctx = ctx();

    let have = caddy.read(&ctx).unwrap();
    assert_eq!(have, Some(state(false, false)));
    let changes = caddy.diff(have.as_ref());
    assert_eq!(changes, [change(Action::Update, "enable, start")]);
    caddy.apply(&ctx, &changes).unwrap();

    let have = caddy.read(&ctx).unwrap();
    assert_eq!(have, Some(state(true, true)));
    assert_eq!(caddy.diff(have.as_ref()), []);
}

#[test]
fn stop_a_running_service() {
    let Some(_systemd) = systemd_host() else {
        return;
    };
    let unit = TestUnit::install("fettle-test-stop", SLEEPER);
    sh(&format!("systemctl enable --now {}", unit.0));
    let apache2 = service(unit.0, Some(false));
    let ctx = ctx();

    let changes = apache2.diff(apache2.read(&ctx).unwrap().as_ref());
    assert_eq!(changes, [change(Action::Update, "disable, stop")]);
    apache2.apply(&ctx, &changes).unwrap();

    assert_eq!(read(unit.0), Some(state(false, false)));
}

#[test]
fn unit_that_cannot_be_enabled() {
    let Some(_systemd) = systemd_host() else {
        return;
    };
    let unit = TestUnit::install(
        "fettle-test-static",
        "[Service]\nExecStart=/bin/sleep infinity\n",
    );
    let foo = service(unit.0, Some(true));
    let ctx = ctx();

    let changes = foo.diff(foo.read(&ctx).unwrap().as_ref());
    let error = foo.apply(&ctx, &changes).unwrap_err().to_string();

    assert!(
        error.starts_with(&format!("service {}: ", unit.0)),
        "{error}"
    );
    assert!(error.contains("static"), "{error}");
}

#[test]
fn unit_still_missing_at_apply() {
    let Some(_systemd) = systemd_host() else {
        return;
    };
    let id = "fettle-test-missing";
    let caddy = service(id, Some(true));

    let error = caddy
        .apply(&ctx(), &caddy.diff(None))
        .unwrap_err()
        .to_string();

    assert!(error.starts_with(&format!("service {id}: ")), "{error}");
}

#[test]
fn remove_a_service_fettle_started() {
    let Some(_systemd) = systemd_host() else {
        return;
    };
    let unit = TestUnit::install("fettle-test-remove", SLEEPER);
    sh(&format!("systemctl enable --now {}", unit.0));

    service(unit.0, None)
        .remove(&ctx(), &state(true, true))
        .unwrap();

    assert_eq!(read(unit.0), Some(state(false, false)));
}

#[test]
fn remove_after_its_package_is_gone() {
    let Some(_systemd) = systemd_host() else {
        return;
    };
    let id = "fettle-test-gone";

    service(id, None)
        .remove(&ctx(), &state(true, true))
        .unwrap();

    assert_eq!(read(id), None);
}

#[test]
fn masked_unit() {
    let Some(_systemd) = systemd_host() else {
        return;
    };
    let unit = TestUnit("fettle-test-masked");
    sh(&format!("systemctl mask {}", unit.0));
    let caddy = service(unit.0, Some(true));
    let ctx = ctx();

    let changes = caddy.diff(caddy.read(&ctx).unwrap().as_ref());
    let error = caddy.apply(&ctx, &changes).unwrap_err().to_string();

    assert!(
        error.starts_with(&format!("service {}: ", unit.0)),
        "{error}"
    );
    assert!(error.contains("masked"), "{error}");
}
