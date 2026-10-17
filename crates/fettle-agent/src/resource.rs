use std::sync::atomic::{AtomicBool, Ordering};

use fettle_protocol::Action;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Host context passed to the operations that touch the host.
#[derive(Debug, Default)]
pub struct Ctx {
    package_lists_fresh: AtomicBool,
}

impl Ctx {
    /// Runs `refresh` unless an earlier call in this run succeeded, so a failed refresh is
    /// retried by the next install.
    pub(crate) fn refresh_package_lists(&self, refresh: impl FnOnce() -> Result<()>) -> Result<()> {
        if self.package_lists_fresh.load(Ordering::Acquire) {
            return Ok(());
        }
        refresh()?;
        self.package_lists_fresh.store(true, Ordering::Release);
        Ok(())
    }
}

/// One step from the state read to the desired state.
///
/// A resource never produces [`Action::Forget`]: forgetting is decided from the recorded origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub action: Action,
    /// Printable text chosen by the resource, such as `+2 −1 lines`.
    pub detail: Option<String>,
}

pub trait Resource {
    /// Observable state (what `read` returns and the state file stores)
    type State: Serialize + DeserializeOwned;

    /// What it looks like now (`None` = does not exist)
    ///
    /// # Errors
    ///
    /// Fails when the host cannot be inspected.
    fn read(&self, ctx: &Ctx) -> Result<Option<Self::State>>;
    /// What needs to change; `self` is the desired state
    fn diff(&self, have: Option<&Self::State>) -> Vec<Change>;
    /// Applies only the difference
    ///
    /// # Errors
    ///
    /// Fails when a change cannot be made on the host.
    fn apply(&self, ctx: &Ctx, changes: &[Change]) -> Result<()>;
    /// Undoes the resource when it leaves the config
    ///
    /// # Errors
    ///
    /// Fails when the resource cannot be undone on the host.
    fn remove(&self, ctx: &Ctx, last: &Self::State) -> Result<()>;
}

/// A [`Resource`] with its state erased to JSON, so resources of different types share one list.
pub trait AnyResource {
    /// # Errors
    ///
    /// Fails when the host cannot be inspected or the state does not serialize.
    fn read(&self, ctx: &Ctx) -> Result<Option<Value>>;
    /// # Errors
    ///
    /// Fails when `have` does not match the resource's state shape.
    fn diff(&self, have: Option<&Value>) -> Result<Vec<Change>>;
    /// # Errors
    ///
    /// Fails when a change cannot be made on the host.
    fn apply(&self, ctx: &Ctx, changes: &[Change]) -> Result<()>;
    /// # Errors
    ///
    /// Fails when `last` does not match the resource's state shape, before the host is touched,
    /// or when the resource cannot be undone.
    fn remove(&self, ctx: &Ctx, last: &Value) -> Result<()>;
}

impl<R: Resource> AnyResource for R {
    fn read(&self, ctx: &Ctx) -> Result<Option<Value>> {
        match Resource::read(self, ctx)? {
            Some(state) => Ok(Some(serde_json::to_value(state)?)),
            None => Ok(None),
        }
    }

    fn diff(&self, have: Option<&Value>) -> Result<Vec<Change>> {
        let have = have.map(typed::<R::State>).transpose()?;
        Ok(Resource::diff(self, have.as_ref()))
    }

    fn apply(&self, ctx: &Ctx, changes: &[Change]) -> Result<()> {
        Resource::apply(self, ctx, changes)
    }

    fn remove(&self, ctx: &Ctx, last: &Value) -> Result<()> {
        Resource::remove(self, ctx, &typed(last)?)
    }
}

fn typed<T: DeserializeOwned>(value: &Value) -> serde_json::Result<T> {
    T::deserialize(value)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn package_lists_refresh_once_after_success() {
        let ctx = Ctx::default();
        let runs = Cell::new(0);
        let refresh = || {
            runs.set(runs.get() + 1);
            Ok(())
        };

        ctx.refresh_package_lists(refresh).unwrap();
        ctx.refresh_package_lists(refresh).unwrap();

        assert_eq!(runs.get(), 1);
    }

    #[test]
    fn failed_refresh_is_retried() {
        let ctx = Ctx::default();
        let runs = Cell::new(0);

        let error = ctx.refresh_package_lists(|| {
            runs.set(runs.get() + 1);
            Err("mirror down".into())
        });
        assert!(error.is_err());
        ctx.refresh_package_lists(|| {
            runs.set(runs.get() + 1);
            Ok(())
        })
        .unwrap();
        ctx.refresh_package_lists(|| {
            runs.set(runs.get() + 1);
            Ok(())
        })
        .unwrap();

        assert_eq!(runs.get(), 2);
    }
}
