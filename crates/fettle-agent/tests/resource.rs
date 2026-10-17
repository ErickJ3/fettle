use std::cell::RefCell;
use std::fmt::Debug;

use fettle_agent::{AnyResource, Change, Ctx, Resource, Result};
use fettle_protocol::Action;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

/// Holds its own "host", since `Ctx` carries nothing yet.
struct Fake<T> {
    desired: T,
    host: RefCell<Option<T>>,
    removed_with: RefCell<Option<T>>,
}

impl<T> Fake<T> {
    fn new(desired: T, host: Option<T>) -> Self {
        Self {
            desired,
            host: RefCell::new(host),
            removed_with: RefCell::new(None),
        }
    }
}

impl<T: Serialize + DeserializeOwned + Clone + PartialEq + Debug> Resource for Fake<T> {
    type State = T;

    fn read(&self, _: &Ctx) -> Result<Option<T>> {
        Ok(self.host.borrow().clone())
    }

    fn diff(&self, have: Option<&T>) -> Vec<Change> {
        match have {
            None => vec![change(Action::Create, None)],
            Some(have) if *have != self.desired => vec![change(
                Action::Update,
                Some(format!("{have:?} -> {:?}", self.desired)),
            )],
            Some(_) => vec![],
        }
    }

    fn apply(&self, _: &Ctx, _: &[Change]) -> Result<()> {
        *self.host.borrow_mut() = Some(self.desired.clone());
        Ok(())
    }

    fn remove(&self, _: &Ctx, last: &T) -> Result<()> {
        *self.removed_with.borrow_mut() = Some(last.clone());
        *self.host.borrow_mut() = None;
        Ok(())
    }
}

fn change(action: Action, detail: Option<String>) -> Change {
    Change { action, detail }
}

fn ctx() -> Ctx {
    Ctx::default()
}

#[test]
fn missing_resource_diffs_to_create() {
    let fake = Fake::new(1u32, None);

    assert_eq!(Resource::diff(&fake, None), [change(Action::Create, None)]);
}

#[test]
fn desired_state_already_holds() {
    let fake = Fake::new(1u32, Some(1));

    assert_eq!(Resource::diff(&fake, Some(&1)), []);
}

#[test]
fn resource_missing_on_the_host() {
    let fake = Box::new(Fake::new(1u32, None));
    let erased: &dyn AnyResource = fake.as_ref();

    assert_eq!(erased.read(&ctx()).unwrap(), None);
    assert_eq!(*fake.host.borrow(), None);
}

#[test]
fn resource_present_on_the_host() {
    let erased: Box<dyn AnyResource> = Box::new(Fake::new(1u32, Some(7)));

    assert_eq!(erased.read(&ctx()).unwrap(), Some(json!(7)));
}

#[test]
fn second_pass_is_empty() {
    let erased: Box<dyn AnyResource> = Box::new(Fake::new(1u32, None));

    let have = erased.read(&ctx()).unwrap();
    let changes = erased.diff(have.as_ref()).unwrap();
    assert_eq!(changes, [change(Action::Create, None)]);
    erased.apply(&ctx(), &changes).unwrap();

    let have = erased.read(&ctx()).unwrap();
    assert_eq!(erased.diff(have.as_ref()).unwrap(), []);
}

#[test]
fn remove_from_a_recorded_state() {
    let fake = Box::new(Fake::new(1u32, Some(7)));
    let erased: &dyn AnyResource = fake.as_ref();

    let recorded = erased.read(&ctx()).unwrap().unwrap();
    erased.remove(&ctx(), &recorded).unwrap();

    assert_eq!(*fake.removed_with.borrow(), Some(7));
    assert_eq!(*fake.host.borrow(), None);
}

#[test]
fn removal_with_a_foreign_state() {
    let fake = Box::new(Fake::new(1u32, Some(7)));
    let erased: &dyn AnyResource = fake.as_ref();

    assert!(erased.remove(&ctx(), &json!("seven")).is_err());
    assert_eq!(*fake.removed_with.borrow(), None);
    assert_eq!(*fake.host.borrow(), Some(7));
}

#[test]
fn diff_with_a_foreign_state() {
    let fake = Box::new(Fake::new(1u32, Some(7)));
    let erased: &dyn AnyResource = fake.as_ref();

    assert!(erased.diff(Some(&json!({ "not": "a number" }))).is_err());
    assert_eq!(*fake.host.borrow(), Some(7));
}

#[test]
fn two_resource_types_in_order() {
    let list: Vec<Box<dyn AnyResource>> = vec![
        Box::new(Fake::new(1u32, Some(1))),
        Box::new(Fake::new("a".to_owned(), Some("b".to_owned()))),
    ];

    let reads: Vec<Option<Value>> = list.iter().map(|r| r.read(&ctx()).unwrap()).collect();

    assert_eq!(reads, [Some(json!(1)), Some(json!("b"))]);
    assert!(reads[0].as_ref().is_some_and(Value::is_number));
    assert!(reads[1].as_ref().is_some_and(Value::is_string));
}
