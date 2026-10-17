use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::process::Command;

use fettle_protocol::Action;
use rustix::fs::{AtFlags, FileType, Gid, Mode, OFlags, Uid};
use rustix::io::Errno;
use serde::{Deserialize, Serialize};

use crate::command::{self, failure};
use crate::{Change, Ctx, Resource, Result};

const AUTHORIZED_KEYS: &str = "authorized_keys";
const TEMP_KEYS: &str = "authorized_keys.fettle-tmp";
const SSH_MODE: Mode = Mode::RWXU;
const KEYS_MODE: Mode = Mode::RUSR.union(Mode::WUSR);

/// An account, managed through `getent`, `useradd`, `usermod` and `userdel`.
#[derive(Debug, Clone)]
pub struct User {
    name: String,
    groups: Option<Vec<String>>,
    ssh_keys: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// Supplementary groups, sorted and deduplicated.
    pub groups: Vec<String>,
    /// Keys in `authorized_keys`, sorted and deduplicated, so a reordered file reads the same.
    pub ssh_keys: Vec<String>,
}

struct Account {
    uid: u32,
    gid: u32,
    home: String,
}

impl User {
    #[must_use]
    pub fn new(id: &str, props: &fettle_protocol::User) -> Self {
        Self {
            name: id.to_owned(),
            groups: props.groups.clone(),
            ssh_keys: props.ssh_keys.clone(),
        }
    }

    /// Checked before any command runs: a leading `-` would read as an option, and a comma
    /// in a group splits it in two in `--groups`.
    fn checked(&self) -> Result<&str> {
        let mut names = std::iter::once(&self.name).chain(self.groups.iter().flatten());
        match names.find(|name| !is_account_name(name)) {
            Some(bad) => Err(self.failed(&format!("{bad} is not a valid account or group name"))),
            None => Ok(&self.name),
        }
    }

    fn account(&self) -> Result<Option<Account>> {
        let name = self.checked()?;
        let mut getent = Command::new("getent");
        getent.args(["passwd", "--", name]);
        let output = command::output(&mut getent).map_err(|e| self.failed(&e))?;
        // Exit status 2 is a key getent did not find.
        if output.status.code() == Some(2) {
            return Ok(None);
        }
        if !output.status.success() {
            return Err(self.failed(&failure(&getent, &output)));
        }
        let entry = String::from_utf8_lossy(&output.stdout);
        let fields: Vec<&str> = entry.trim_end().split(':').collect();
        match fields[..] {
            [_, _, uid, gid, _, home, ..] => Ok(Some(Account {
                uid: uid
                    .parse()
                    .map_err(|e| self.failed(&format!("uid {uid}: {e}")))?,
                gid: gid
                    .parse()
                    .map_err(|e| self.failed(&format!("gid {gid}: {e}")))?,
                home: home.to_owned(),
            })),
            _ => Err(self.failed(&format!("unexpected passwd entry {entry:?}"))),
        }
    }

    /// From `getent group` rather than `id -Gn`, which folds the primary group in, so a declared
    /// group equal to it could never converge.
    fn supplementary_groups(&self) -> Result<Vec<String>> {
        let groups =
            command::run(Command::new("getent").arg("group")).map_err(|e| self.failed(&e))?;
        Ok(groups
            .lines()
            .filter_map(|line| {
                let fields: Vec<&str> = line.split(':').collect();
                let members = fields.get(3)?;
                members
                    .split(',')
                    .any(|member| member == self.name)
                    .then(|| fields[0].to_owned())
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect())
    }

    fn create(&self) -> Result<()> {
        let mut useradd = Command::new("useradd");
        useradd.arg("--create-home");
        if let Some(groups) = self.groups.as_ref().filter(|groups| !groups.is_empty()) {
            useradd.args(["--groups", &groups.join(",")]);
        }
        command::run(useradd.args(["--", self.checked()?])).map_err(|e| self.failed(&e))?;
        Ok(())
    }

    fn set_groups(&self) -> Result<()> {
        let Some(groups) = &self.groups else {
            return Ok(());
        };
        // An empty argument is no supplementary groups to shadow-utils.
        command::run(Command::new("usermod").args([
            "--groups",
            &groups.join(","),
            "--",
            self.checked()?,
        ]))
        .map_err(|e| self.failed(&e))?;
        Ok(())
    }

    fn set_keys(&self) -> Result<()> {
        let Some(keys) = &self.ssh_keys else {
            return Ok(());
        };
        let account = self
            .account()?
            .ok_or_else(|| self.failed(&"account does not exist"))?;
        let content: String = keys.iter().flat_map(|key| [key.trim(), "\n"]).collect();
        write_keys(&account, &content).map_err(|e| self.failed(&e))
    }

    fn failed(&self, error: &dyn std::fmt::Display) -> Box<dyn std::error::Error + Send + Sync> {
        format!("user {}: {error}", self.name).into()
    }
}

/// `^[a-z_][a-z0-9_-]*\$?$`, at most 32 characters: what Debian's `useradd` accepts by default.
fn is_account_name(name: &str) -> bool {
    let body = name.strip_suffix('$').unwrap_or(name);
    name.len() <= 32
        && body.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
        && body
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_-".contains(c))
}

impl Resource for User {
    type State = State;

    fn read(&self, _: &Ctx) -> Result<Option<State>> {
        let Some(account) = self.account()? else {
            return Ok(None);
        };
        let keys = read_keys(&account).map_err(|e| self.failed(&e))?;
        Ok(Some(State {
            groups: self.supplementary_groups()?,
            ssh_keys: keys.into_iter().collect(),
        }))
    }

    fn diff(&self, have: Option<&State>) -> Vec<Change> {
        let Some(have) = have else {
            return vec![Change {
                action: Action::Create,
                detail: None,
            }];
        };
        let mut parts = Vec::new();
        if let Some(groups) = &self.groups {
            let (added, removed) = differences(groups.iter().map(String::as_str), &have.groups);
            if !added.is_empty() || !removed.is_empty() {
                let added = added.iter().map(|group| format!(" +{group}"));
                let removed = removed.iter().map(|group| format!(" \u{2212}{group}"));
                parts.push(format!(
                    "groups{}",
                    added.chain(removed).collect::<String>()
                ));
            }
        }
        if let Some(keys) = &self.ssh_keys {
            let (added, removed) = differences(keys.iter().map(|key| key.trim()), &have.ssh_keys);
            if !added.is_empty() || !removed.is_empty() {
                parts.push(format!(
                    "ssh keys +{} \u{2212}{}",
                    added.len(),
                    removed.len()
                ));
            }
        }
        if parts.is_empty() {
            return vec![];
        }
        vec![Change {
            action: Action::Update,
            detail: Some(parts.join(", ")),
        }]
    }

    /// Sets every declared prop, not only those the detail names: each step is idempotent.
    fn apply(&self, _: &Ctx, changes: &[Change]) -> Result<()> {
        for change in changes {
            match change.action {
                Action::Create => self.create()?,
                Action::Update => self.set_groups()?,
                ref action => return Err(self.failed(&format!("cannot apply {action:?}"))),
            }
            self.set_keys()?;
        }
        Ok(())
    }

    /// Keeps the home directory: it holds the user's data, which fettle never declared.
    fn remove(&self, _: &Ctx, _: &State) -> Result<()> {
        if self.account()?.is_none() {
            return Ok(());
        }
        command::run(Command::new("userdel").args(["--", self.checked()?]))
            .map_err(|e| self.failed(&e))?;
        Ok(())
    }
}

/// Sorted additions and removals that turn `have` into `want`.
fn differences<'a>(
    want: impl Iterator<Item = &'a str>,
    have: &'a [String],
) -> (Vec<&'a str>, Vec<&'a str>) {
    let want: BTreeSet<&str> = want.collect();
    let have: BTreeSet<&str> = have.iter().map(String::as_str).collect();
    (
        want.difference(&have).copied().collect(),
        have.difference(&want).copied().collect(),
    )
}

// The agent is root and `~/.ssh` belongs to the user, who can swap it for a link between any
// check and any write; so past the root-owned home every step goes through directory file
// descriptors and never follows a link.

fn read_keys(account: &Account) -> std::result::Result<BTreeSet<String>, String> {
    let ssh_path = format!("{}/.ssh", account.home);
    let keys_path = format!("{ssh_path}/{AUTHORIZED_KEYS}");
    let Some(home) = open_home(account).map_err(|e| at(&account.home, e))? else {
        return Ok(BTreeSet::new());
    };
    let ssh = match open_dir_at(&home, ".ssh") {
        Err(Errno::NOENT) => return Ok(BTreeSet::new()),
        ssh => ssh.map_err(|e| at(&ssh_path, e))?,
    };
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let file = match rustix::fs::openat(&ssh, AUTHORIZED_KEYS, flags, Mode::empty()) {
        Err(Errno::NOENT) => return Ok(BTreeSet::new()),
        file => file.map_err(|e| at(&keys_path, e))?,
    };
    let stat = rustix::fs::fstat(&file).map_err(|e| at(&keys_path, e))?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return Err(format!("{keys_path}: not a regular file"));
    }
    let mut bytes = Vec::new();
    fs::File::from(file)
        .read_to_end(&mut bytes)
        .map_err(|e| at(&keys_path, e))?;
    Ok(String::from_utf8_lossy(&bytes)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect())
}

fn write_keys(account: &Account, content: &str) -> std::result::Result<(), String> {
    let ssh_path = format!("{}/.ssh", account.home);
    let temp_path = format!("{ssh_path}/{TEMP_KEYS}");
    let home = open_home(account)
        .map_err(|e| at(&account.home, e))?
        .ok_or_else(|| format!("home directory {} does not exist", account.home))?;
    let ssh = open_or_create_ssh(account, &home).map_err(|e| at(&ssh_path, e))?;
    match rustix::fs::unlinkat(&ssh, TEMP_KEYS, AtFlags::empty()) {
        Ok(()) | Err(Errno::NOENT) => {}
        Err(e) => return Err(at(&temp_path, e)),
    }
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let temp =
        rustix::fs::openat(&ssh, TEMP_KEYS, flags, KEYS_MODE).map_err(|e| at(&temp_path, e))?;
    let written = fill(account, temp, content).and_then(|()| {
        Ok(rustix::fs::renameat(
            &ssh,
            TEMP_KEYS,
            &ssh,
            AUTHORIZED_KEYS,
        )?)
    });
    if let Err(e) = written {
        let _ = rustix::fs::unlinkat(&ssh, TEMP_KEYS, AtFlags::empty());
        return Err(at(&format!("{ssh_path}/{AUTHORIZED_KEYS}"), e));
    }
    Ok(())
}

fn fill(account: &Account, temp: OwnedFd, content: &str) -> io::Result<()> {
    let mut file = fs::File::from(temp);
    file.write_all(content.as_bytes())?;
    own(&file, account, KEYS_MODE)?;
    file.sync_all()
}

/// The home path comes from `passwd` and its parent is root-owned, so following links there is
/// safe. `None` is a home directory that does not exist.
fn open_home(account: &Account) -> rustix::io::Result<Option<OwnedFd>> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    match rustix::fs::open(&account.home, flags, Mode::empty()) {
        Err(Errno::NOENT) => Ok(None),
        home => home.map(Some),
    }
}

fn open_dir_at(dir: &OwnedFd, name: &str) -> rustix::io::Result<OwnedFd> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    rustix::fs::openat(dir, name, flags, Mode::empty())
}

/// An existing `.ssh` is kept as is; only one fettle creates gets the user's ownership.
fn open_or_create_ssh(account: &Account, home: &OwnedFd) -> rustix::io::Result<OwnedFd> {
    match open_dir_at(home, ".ssh") {
        Err(Errno::NOENT) => {
            rustix::fs::mkdirat(home, ".ssh", SSH_MODE)?;
            let ssh = open_dir_at(home, ".ssh")?;
            // The umask may have narrowed the mode given to mkdirat.
            own(&ssh, account, SSH_MODE)?;
            Ok(ssh)
        }
        ssh => ssh,
    }
}

/// The chown comes first because it clears setuid and setgid bits set before it.
fn own(fd: impl AsFd, account: &Account, mode: Mode) -> rustix::io::Result<()> {
    rustix::fs::fchown(
        &fd,
        Some(Uid::from_raw(account.uid)),
        Some(Gid::from_raw(account.gid)),
    )?;
    rustix::fs::fchmod(&fd, mode)
}

fn at(path: &str, error: impl Into<io::Error>) -> String {
    let error = error.into();
    if error.raw_os_error() == Some(Errno::LOOP.raw_os_error()) {
        return format!("{path}: is a symbolic link, not followed");
    }
    format!("{path}: {error}")
}
