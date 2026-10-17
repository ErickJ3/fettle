use std::process::Command;

use fettle_protocol::Action;
use serde::{Deserialize, Serialize};

use crate::command::{self, failure};
use crate::{Change, Ctx, Resource, Result};

/// A package on a Debian-family host, managed through `dpkg-query` and `apt-get`.
#[derive(Debug, Clone)]
pub struct Pkg {
    name: String,
    absent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// Recorded for the state file; `diff` ignores it since fettle does not manage versions.
    pub version: String,
}

impl Pkg {
    #[must_use]
    pub fn new(id: &str, props: &fettle_protocol::Pkg) -> Self {
        Self {
            name: id.to_owned(),
            absent: props.absent,
        }
    }

    /// Checked before any command runs: apt would read `name=1.0` or `name/suite` as a pin
    /// that `dpkg-query` never finds under that name, and a leading `-` as an option.
    fn checked(&self) -> Result<&str> {
        let (base, arch) = match self.name.split_once(':') {
            Some((base, arch)) => (base, Some(arch)),
            None => (self.name.as_str(), None),
        };
        let valid_base = base.len() >= 2
            && base.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
            && base
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "+-.".contains(c));
        let valid_arch = arch.is_none_or(|arch| {
            !arch.is_empty()
                && arch
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        });
        if valid_base && valid_arch {
            Ok(&self.name)
        } else {
            Err(format!("pkg {}: not a Debian package name", self.name).into())
        }
    }

    fn installed_version(&self) -> Result<Option<String>> {
        let name = self.checked()?;
        let mut query = Command::new("dpkg-query");
        query.args(["-W", "-f", "${db:Status-Abbrev}${Version}\n", "--", name]);
        let output = command::output(&mut query).map_err(|e| self.failed(&e))?;
        // Exit status 1 with no output is a package dpkg has never heard of.
        if output.status.code() == Some(1) && output.stdout.is_empty() {
            return Ok(None);
        }
        if !output.status.success() {
            return Err(self.failed(&failure(&query, &output)));
        }
        // A multiarch package prints one line per architecture.
        let status = String::from_utf8_lossy(&output.stdout);
        Ok(status
            .lines()
            .find_map(|line| line.strip_prefix("ii "))
            .map(str::to_owned))
    }

    fn install(&self, ctx: &Ctx) -> Result<()> {
        let name = self.checked()?;
        ctx.refresh_package_lists(|| command::run(&mut apt_get(&["update"])).map(drop))
            .map_err(|e| self.failed(&e))?;
        let mut install = apt_get(&[
            "install",
            "-y",
            "--no-install-recommends",
            "-o",
            "Dpkg::Options::=--force-confdef",
            "-o",
            "Dpkg::Options::=--force-confold",
            "--",
            name,
        ]);
        command::run(&mut install).map_err(|e| self.failed(&e))?;
        Ok(())
    }

    fn uninstall(&self) -> Result<()> {
        let name = self.checked()?;
        command::run(&mut apt_get(&["remove", "-y", "--", name])).map_err(|e| self.failed(&e))?;
        Ok(())
    }

    fn failed(&self, error: &dyn std::fmt::Display) -> Box<dyn std::error::Error + Send + Sync> {
        format!("pkg {}: {error}", self.name).into()
    }
}

impl Resource for Pkg {
    type State = State;

    fn read(&self, _: &Ctx) -> Result<Option<State>> {
        Ok(self.installed_version()?.map(|version| State { version }))
    }

    fn diff(&self, have: Option<&State>) -> Vec<Change> {
        let action = match (self.absent, have) {
            (false, None) => Action::Create,
            (true, Some(_)) => Action::Remove,
            _ => return vec![],
        };
        vec![Change {
            action,
            detail: None,
        }]
    }

    fn apply(&self, ctx: &Ctx, changes: &[Change]) -> Result<()> {
        for change in changes {
            match change.action {
                Action::Create => self.install(ctx)?,
                Action::Remove => self.uninstall()?,
                ref action => return Err(self.failed(&format!("cannot apply {action:?}"))),
            }
        }
        Ok(())
    }

    fn remove(&self, _: &Ctx, _: &State) -> Result<()> {
        // apt-get fails on a name its lists no longer carry, so a package already gone is
        // checked first.
        if self.installed_version()?.is_none() {
            return Ok(());
        }
        self.uninstall()
    }
}

/// The lock timeout waits for `unattended-upgrades`, which holds the lock on fresh cloud hosts.
/// 60 s outlasts one of its dpkg steps, not a whole run; a longer hold fails with apt's lock
/// message.
fn apt_get(args: &[&str]) -> Command {
    let mut command = Command::new("apt-get");
    command
        .env("DEBIAN_FRONTEND", "noninteractive")
        .args(["-o", "DPkg::Lock::Timeout=60"])
        .args(args);
    command
}
