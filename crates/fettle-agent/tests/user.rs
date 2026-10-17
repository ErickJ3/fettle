use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use fettle_agent::user::{State, User};
use fettle_agent::{Change, Ctx, Resource};
use fettle_protocol::Action;

const KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFettleTestKeyOne fettle@test";
const OTHER_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFettleTestKeyTwo fettle@test";

/// `useradd` and friends fail on a locked `/etc/passwd`, so host tests run one at a time.
static ACCOUNTS: Mutex<()> = Mutex::new(());

fn user(name: &str, groups: Option<&[&str]>, ssh_keys: Option<&[&str]>) -> User {
    let owned = |list: &[&str]| list.iter().map(|&s| s.to_owned()).collect();
    User::new(
        name,
        &fettle_protocol::User {
            groups: groups.map(owned),
            ssh_keys: ssh_keys.map(owned),
        },
    )
}

fn state(groups: &[&str], ssh_keys: &[&str]) -> State {
    let owned = |list: &[&str]| list.iter().map(|&s| s.to_owned()).collect();
    State {
        groups: owned(groups),
        ssh_keys: owned(ssh_keys),
    }
}

fn update(detail: &str) -> Change {
    Change {
        action: Action::Update,
        detail: Some(detail.to_owned()),
    }
}

fn ctx() -> Ctx {
    Ctx::default()
}

#[test]
fn name_that_looks_like_an_option() {
    let error = user("--help", None, None)
        .read(&ctx())
        .unwrap_err()
        .to_string();

    // Any command run would fail first, naming it; on macOS getent does not even exist.
    assert_eq!(
        error,
        "user --help: --help is not a valid account or group name"
    );
}

#[test]
fn group_with_a_comma() {
    let error = user("marcos", Some(&["docker,sudo"]), None)
        .apply(&ctx(), &[update("groups +docker,sudo")])
        .unwrap_err()
        .to_string();

    assert_eq!(
        error,
        "user marcos: docker,sudo is not a valid account or group name"
    );
}

#[test]
fn names_useradd_would_misread() {
    let long = "a".repeat(33);
    for name in ["Marcos", "1abc", "a b", "a$b", "", long.as_str()] {
        let error = user(name, None, None).read(&ctx()).unwrap_err().to_string();
        assert!(
            error.ends_with("is not a valid account or group name"),
            "{name}: {error}"
        );
    }
}

#[test]
fn create_has_no_detail() {
    assert_eq!(
        user("marcos", Some(&["docker"]), Some(&[KEY])).diff(None),
        [Change {
            action: Action::Create,
            detail: None,
        }]
    );
}

#[test]
fn groups_omitted() {
    assert_eq!(
        user("marcos", None, None).diff(Some(&state(&["sudo"], &[KEY]))),
        []
    );
}

#[test]
fn same_keys_in_another_order() {
    let declared = format!("{OTHER_KEY}\n");
    let marcos = user("marcos", Some(&["sudo"]), Some(&[&declared, KEY, KEY]));
    let mut keys = [KEY, OTHER_KEY];
    keys.sort_unstable();

    assert_eq!(marcos.diff(Some(&state(&["sudo"], &keys))), []);
}

#[test]
fn groups_and_keys_change() {
    let marcos = user(
        "marcos",
        Some(&["docker"]),
        Some(&["ssh-ed25519 AAAA two", "ssh-ed25519 BBBB three"]),
    );

    assert_eq!(
        marcos.diff(Some(&state(&["sudo"], &["ssh-ed25519 CCCC one"]))),
        [update("groups +docker \u{2212}sudo, ssh keys +2 \u{2212}1")]
    );
}

#[test]
fn empty_lists_clear() {
    let marcos = user("marcos", Some(&[]), Some(&[]));

    assert_eq!(
        marcos.diff(Some(&state(&["adm", "sudo"], &[KEY]))),
        [update(
            "groups \u{2212}adm \u{2212}sudo, ssh keys +0 \u{2212}1"
        )]
    );
}

/// `None` unless `FETTLE_TEST_HOST` is set; set without root it panics, so a misconfigured CI
/// step fails instead of passing empty.
fn accounts_host() -> Option<MutexGuard<'static, ()>> {
    std::env::var_os("FETTLE_TEST_HOST")?;
    let uid = fs::metadata("/proc/self").map(|m| m.uid());
    assert_eq!(uid.ok(), Some(0), "FETTLE_TEST_HOST needs root");
    Some(ACCOUNTS.lock().unwrap_or_else(PoisonError::into_inner))
}

fn sh(script: &str) {
    let status = Command::new("sh").args(["-c", script]).status().unwrap();
    assert!(status.success(), "{script}");
}

/// An account name that is deleted, with its home, when dropped.
struct TestAccount(&'static str);

impl TestAccount {
    /// Starts from no account, in case an earlier run left one behind.
    fn missing(name: &'static str) -> Self {
        let account = Self(name);
        account.delete();
        account
    }

    /// Created with `useradd`, not fettle, and a home directory.
    fn existing(name: &'static str, groups: &str) -> Self {
        let account = Self::missing(name);
        let groups = if groups.is_empty() {
            String::new()
        } else {
            format!("--groups {groups}")
        };
        sh(&format!("useradd --create-home {groups} {name}"));
        account
    }

    fn home(&self) -> String {
        format!("/home/{}", self.0)
    }

    fn authorized_keys(&self) -> String {
        format!("/home/{}/.ssh/authorized_keys", self.0)
    }

    fn delete(&self) {
        let name = self.0;
        let _ = Command::new("sh")
            .args([
                "-c",
                &format!("pkill -KILL -u {name}; userdel {name}; rm -rf /home/{name}"),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

impl Drop for TestAccount {
    fn drop(&mut self) {
        self.delete();
    }
}

fn read(name: &str) -> Option<State> {
    user(name, None, None).read(&ctx()).unwrap()
}

#[test]
fn existing_account() {
    let Some(_host) = accounts_host() else { return };
    let account = TestAccount::existing("fettle-test-existing", "adm,users");
    sh(&format!(
        "mkdir -p {home}/.ssh && echo '{KEY}' > {keys}",
        home = account.home(),
        keys = account.authorized_keys()
    ));

    assert_eq!(read(account.0), Some(state(&["adm", "users"], &[KEY])));
}

#[test]
fn missing_account() {
    let Some(_host) = accounts_host() else { return };
    let account = TestAccount::missing("fettle-test-missing");

    assert_eq!(read(account.0), None);
}

#[test]
fn comment_lines_are_not_keys() {
    let Some(_host) = accounts_host() else { return };
    let account = TestAccount::existing("fettle-test-comments", "");
    sh(&format!("mkdir {}/.ssh", account.home()));
    fs::write(
        account.authorized_keys(),
        format!("# laptop\n\n  {KEY}  \n#{OTHER_KEY}\n"),
    )
    .unwrap();

    assert_eq!(read(account.0), Some(state(&[], &[KEY])));
}

#[test]
fn create_and_converge() {
    let Some(_host) = accounts_host() else { return };
    let account = TestAccount::missing("fettle-test-create");
    let declared = format!("{KEY}\n");
    let fettle_test = user(account.0, Some(&["adm"]), Some(&[&declared]));
    let ctx = ctx();

    let have = fettle_test.read(&ctx).unwrap();
    assert_eq!(have, None);
    let changes = fettle_test.diff(have.as_ref());
    assert_eq!(changes[0].action, Action::Create);
    fettle_test.apply(&ctx, &changes).unwrap();

    let have = fettle_test.read(&ctx).unwrap();
    assert_eq!(have, Some(state(&["adm"], &[KEY])));
    assert!(Path::new(&account.home()).is_dir());
    assert_eq!(fettle_test.diff(have.as_ref()), []);
}

#[test]
fn group_added_and_group_dropped() {
    let Some(_host) = accounts_host() else { return };
    let account = TestAccount::existing("fettle-test-groups", "adm");
    let marcos = user(account.0, Some(&["users"]), None);
    let ctx = ctx();

    let changes = marcos.diff(marcos.read(&ctx).unwrap().as_ref());
    assert_eq!(changes, [update("groups +users \u{2212}adm")]);
    marcos.apply(&ctx, &changes).unwrap();

    assert_eq!(read(account.0), Some(state(&["users"], &[])));
}

#[test]
fn group_that_does_not_exist() {
    let Some(_host) = accounts_host() else { return };
    let account = TestAccount::existing("fettle-test-nogroup", "");
    let marcos = user(account.0, Some(&["fettle-no-group"]), None);
    let ctx = ctx();

    let changes = marcos.diff(marcos.read(&ctx).unwrap().as_ref());
    let error = marcos.apply(&ctx, &changes).unwrap_err().to_string();

    assert!(
        error.starts_with(&format!("user {}: ", account.0)),
        "{error}"
    );
    assert!(error.contains("usermod"), "{error}");
    assert!(error.contains("'fettle-no-group'"), "{error}");
}

#[test]
fn key_added_by_hand_is_removed() {
    let Some(_host) = accounts_host() else { return };
    let account = TestAccount::existing("fettle-test-handkey", "");
    sh(&format!("mkdir {}/.ssh", account.home()));
    fs::write(account.authorized_keys(), format!("{KEY}\n{OTHER_KEY}\n")).unwrap();
    let marcos = user(account.0, None, Some(&[KEY]));
    let ctx = ctx();

    let changes = marcos.diff(marcos.read(&ctx).unwrap().as_ref());
    assert_eq!(changes, [update("ssh keys +0 \u{2212}1")]);
    marcos.apply(&ctx, &changes).unwrap();

    assert_eq!(
        fs::read_to_string(account.authorized_keys()).unwrap(),
        format!("{KEY}\n")
    );
}

#[test]
fn first_key_for_an_account() {
    let Some(_host) = accounts_host() else { return };
    let account = TestAccount::existing("fettle-test-firstkey", "");
    let marcos = user(account.0, None, Some(&[KEY]));
    let ctx = ctx();

    let changes = marcos.diff(marcos.read(&ctx).unwrap().as_ref());
    marcos.apply(&ctx, &changes).unwrap();

    let home = fs::metadata(account.home()).unwrap();
    let ssh = fs::symlink_metadata(format!("{}/.ssh", account.home())).unwrap();
    let keys = fs::symlink_metadata(account.authorized_keys()).unwrap();
    assert!(ssh.is_dir() && keys.is_file());
    assert_eq!(ssh.permissions().mode() & 0o7777, 0o700);
    assert_eq!(keys.permissions().mode() & 0o7777, 0o600);
    for owned in [&ssh, &keys] {
        assert_eq!((owned.uid(), owned.gid()), (home.uid(), home.gid()));
    }
}

#[test]
fn stale_temp_file_does_not_block_a_write() {
    let Some(_host) = accounts_host() else { return };
    let account = TestAccount::existing("fettle-test-staletmp", "");
    let temp = format!("{}/.ssh/authorized_keys.fettle-tmp", account.home());
    sh(&format!(
        "mkdir {}/.ssh && echo partial > {temp}",
        account.home()
    ));
    let marcos = user(account.0, None, Some(&[KEY]));
    let ctx = ctx();

    let changes = marcos.diff(marcos.read(&ctx).unwrap().as_ref());
    marcos.apply(&ctx, &changes).unwrap();

    assert_eq!(read(account.0), Some(state(&[], &[KEY])));
    assert!(!Path::new(&temp).exists());
}

#[test]
fn authorized_keys_points_elsewhere() {
    let Some(_host) = accounts_host() else { return };
    let account = TestAccount::existing("fettle-test-keylink", "");
    sh(&format!(
        "mkdir {}/.ssh && ln -s /etc/shadow {}",
        account.home(),
        account.authorized_keys()
    ));

    let error = user(account.0, None, None)
        .read(&ctx())
        .unwrap_err()
        .to_string();

    assert!(error.contains(&account.authorized_keys()), "{error}");
    assert!(!error.contains("root:"), "{error}");
}

#[test]
fn ssh_directory_link_is_not_followed() {
    let Some(_host) = accounts_host() else { return };
    let account = TestAccount::existing("fettle-test-sshlink", "");
    let elsewhere = tempfile::TempDir::new().unwrap();
    let ssh = format!("{}/.ssh", account.home());
    sh(&format!("ln -s {} {ssh}", elsewhere.path().display()));
    let marcos = user(account.0, None, Some(&[KEY]));
    let ctx = ctx();

    let read_error = marcos.read(&ctx).unwrap_err().to_string();
    let write_error = marcos
        .apply(&ctx, &[update("ssh keys +1 \u{2212}0")])
        .unwrap_err()
        .to_string();

    for error in [read_error, write_error] {
        assert!(error.contains(&format!("{ssh}:")), "{error}");
    }
    assert_eq!(fs::read_dir(elsewhere.path()).unwrap().count(), 0);
}

#[test]
fn remove_an_account_fettle_created() {
    let Some(_host) = accounts_host() else { return };
    let account = TestAccount::missing("fettle-test-remove");
    let fettle_test = user(account.0, None, None);
    let ctx = ctx();
    fettle_test.apply(&ctx, &fettle_test.diff(None)).unwrap();
    let recorded = fettle_test.read(&ctx).unwrap().unwrap();

    user(account.0, None, None).remove(&ctx, &recorded).unwrap();

    assert_eq!(read(account.0), None);
    assert!(Path::new(&account.home()).is_dir());
}

#[test]
fn remove_an_account_already_gone() {
    let Some(_host) = accounts_host() else { return };
    let account = TestAccount::missing("fettle-test-gone");

    user(account.0, None, None)
        .remove(&ctx(), &state(&[], &[]))
        .unwrap();

    assert_eq!(read(account.0), None);
}

#[test]
fn account_with_running_processes() {
    let Some(_host) = accounts_host() else { return };
    let account = TestAccount::existing("fettle-test-busy", "");
    let mut sleeper = Command::new("runuser")
        .args(["-u", account.0, "--", "sleep", "60"])
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !Command::new("pgrep")
        .args(["-u", account.0, "sleep"])
        .stdout(Stdio::null())
        .status()
        .unwrap()
        .success()
    {
        assert!(Instant::now() < deadline, "sleep never started");
        std::thread::sleep(Duration::from_millis(50));
    }

    let error = user(account.0, None, None)
        .remove(&ctx(), &state(&[], &[]))
        .unwrap_err()
        .to_string();
    sleeper.kill().unwrap();
    sleeper.wait().unwrap();

    assert!(
        error.starts_with(&format!("user {}: ", account.0)),
        "{error}"
    );
    assert!(error.contains("userdel"), "{error}");
}
