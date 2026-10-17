use std::process::Command;

use fettle_protocol::Action;
use serde::{Deserialize, Serialize};

use crate::command;
use crate::{Change, Ctx, Resource, Result};

/// A systemd unit, managed through `systemctl`.
#[derive(Debug, Clone)]
pub struct Service {
    unit: String,
    enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub enabled: bool,
    pub running: bool,
}

impl State {
    /// Reads the output of `systemctl show --property=LoadState,UnitFileState,ActiveState`;
    /// `None` is a unit systemd cannot find.
    #[must_use]
    pub fn from_systemctl_show(output: &str) -> Option<Self> {
        Unit::parse(output).state()
    }
}

/// The raw properties, kept for the error when apply does not converge.
#[derive(Debug, Default)]
struct Unit {
    load: String,
    file: String,
    active: String,
}

impl Unit {
    /// By key, since systemd prints properties in its own order.
    fn parse(output: &str) -> Self {
        let mut unit = Self::default();
        for (key, value) in output.lines().filter_map(|line| line.split_once('=')) {
            let field = match key {
                "LoadState" => &mut unit.load,
                "UnitFileState" => &mut unit.file,
                "ActiveState" => &mut unit.active,
                _ => continue,
            };
            value.clone_into(field);
        }
        unit
    }

    fn state(&self) -> Option<State> {
        (self.load != "not-found").then(|| State {
            enabled: self.file == "enabled",
            running: matches!(self.active.as_str(), "active" | "reloading"),
        })
    }
}

impl Service {
    #[must_use]
    pub fn new(id: &str, props: &fettle_protocol::Service) -> Self {
        Self {
            unit: id.to_owned(),
            enabled: props.enabled,
        }
    }

    fn unit(&self) -> Result<Unit> {
        let output = command::run(Command::new("systemctl").args([
            "show",
            "--property=LoadState,UnitFileState,ActiveState",
            "--",
            &self.unit,
        ]))
        .map_err(|e| self.failed(&e))?;
        Ok(Unit::parse(&output))
    }

    /// `enable --now` and `disable --now` are idempotent, so one command covers every change.
    fn converge(&self, enabled: bool) -> Result<()> {
        let verb = if enabled { "enable" } else { "disable" };
        command::run(Command::new("systemctl").args([verb, "--now", "--", &self.unit]))
            .map_err(|e| self.failed(&e))?;
        self.check(enabled, verb)
    }

    /// A static unit cannot be enabled and a oneshot unit stops once it ran, both without
    /// `systemctl` failing; without this check every plan would show the same update.
    fn check(&self, enabled: bool, verb: &str) -> Result<()> {
        let unit = self.unit()?;
        let want = State {
            enabled,
            running: enabled,
        };
        if unit.state() == Some(want) {
            return Ok(());
        }
        Err(self.failed(&format!(
            "{}, {} after systemctl {verb} --now",
            unit.file, unit.active
        )))
    }

    fn failed(&self, error: &dyn std::fmt::Display) -> Box<dyn std::error::Error + Send + Sync> {
        format!("service {}: {error}", self.unit).into()
    }
}

impl Resource for Service {
    type State = State;

    fn read(&self, _: &Ctx) -> Result<Option<State>> {
        Ok(self.unit()?.state())
    }

    fn diff(&self, have: Option<&State>) -> Vec<Change> {
        let Some(enabled) = self.enabled else {
            return vec![];
        };
        let (action, have) = match have {
            Some(have) => (Action::Update, have),
            None if enabled => (
                Action::Create,
                &State {
                    enabled: false,
                    running: false,
                },
            ),
            None => return vec![],
        };
        let mut steps = Vec::new();
        if have.enabled != enabled {
            steps.push(if enabled { "enable" } else { "disable" });
        }
        if have.running != enabled {
            steps.push(if enabled { "start" } else { "stop" });
        }
        if steps.is_empty() {
            return vec![];
        }
        vec![Change {
            action,
            detail: Some(steps.join(", ")),
        }]
    }

    fn apply(&self, _: &Ctx, changes: &[Change]) -> Result<()> {
        for change in changes {
            match (&change.action, self.enabled) {
                (Action::Create | Action::Update, Some(enabled)) => self.converge(enabled)?,
                (action, _) => return Err(self.failed(&format!("cannot apply {action:?}"))),
            }
        }
        Ok(())
    }

    fn remove(&self, _: &Ctx, _: &State) -> Result<()> {
        if self.unit()?.state().is_none() {
            return Ok(());
        }
        command::run(Command::new("systemctl").args(["disable", "--now", "--", &self.unit]))
            .map_err(|e| self.failed(&e))?;
        Ok(())
    }
}
