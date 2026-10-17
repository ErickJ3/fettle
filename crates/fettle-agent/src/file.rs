use std::fs::{self, Permissions};
use std::io::{self, ErrorKind, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt, fchown};
use std::path::{Path, PathBuf};

use fettle_protocol::Action;
use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};
use tempfile::NamedTempFile;

use crate::{Change, Ctx, Resource, Result};

const NEW_FILE_MODE: u32 = 0o644;

/// A file whose text is declared in `content`.
#[derive(Debug, Clone)]
pub struct File {
    path: PathBuf,
    desired: fettle_protocol::File,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub content: String,
}

impl File {
    #[must_use]
    pub fn new(id: &str, props: &fettle_protocol::File) -> Self {
        Self {
            path: id.into(),
            desired: props.clone(),
        }
    }
}

impl Resource for File {
    type State = State;

    fn read(&self, _: &Ctx) -> Result<Option<State>> {
        let path = &self.path;
        match fs::metadata(path) {
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(at(path, &error)),
            Ok(metadata) if !metadata.is_file() => {
                Err(format!("{}: not a regular file", path.display()).into())
            }
            Ok(_) => {
                let bytes = fs::read(path).map_err(|e| at(path, &e))?;
                Ok(Some(State {
                    content: String::from_utf8_lossy(&bytes).into_owned(),
                }))
            }
        }
    }

    fn diff(&self, have: Option<&State>) -> Vec<Change> {
        match (&self.desired, have) {
            (fettle_protocol::File::Present { content }, None) => vec![Change {
                action: Action::Create,
                detail: Some(format!("+{} lines", content.lines().count())),
            }],
            (fettle_protocol::File::Present { content }, Some(have))
                if have.content != *content =>
            {
                vec![Change {
                    action: Action::Update,
                    detail: Some(line_counts(&have.content, content)),
                }]
            }
            (fettle_protocol::File::Absent, Some(_)) => vec![Change {
                action: Action::Remove,
                detail: None,
            }],
            _ => vec![],
        }
    }

    fn apply(&self, _: &Ctx, changes: &[Change]) -> Result<()> {
        for change in changes {
            match (&change.action, &self.desired) {
                (Action::Create | Action::Update, fettle_protocol::File::Present { content }) => {
                    write(&self.path, content)?;
                }
                (Action::Remove, _) => delete(&self.path)?,
                (action, _) => {
                    return Err(
                        format!("file {}: cannot apply {action:?}", self.path.display()).into(),
                    );
                }
            }
        }
        Ok(())
    }

    fn remove(&self, _: &Ctx, _: &State) -> Result<()> {
        delete(&self.path)
    }
}

fn line_counts(old: &str, new: &str) -> String {
    let diff = TextDiff::from_lines(old, new);
    let count = |tag| diff.iter_all_changes().filter(|c| c.tag() == tag).count();
    format!(
        "+{} \u{2212}{} lines",
        count(ChangeTag::Insert),
        count(ChangeTag::Delete)
    )
}

/// Replaces the file through a rename in its directory, so readers never see a partial write.
fn write(path: &Path, content: &str) -> Result<()> {
    let target = match fs::canonicalize(path) {
        Ok(target) => target,
        Err(error) if error.kind() == ErrorKind::NotFound => match fs::read_link(path) {
            // A dangling link stays a link: the file is created where it points.
            Ok(dest) => path.parent().unwrap_or(Path::new("")).join(dest),
            Err(_) => path.to_owned(),
        },
        Err(error) => return Err(at(path, &error)),
    };
    let parent = target
        .parent()
        .ok_or_else(|| format!("{}: has no parent directory", path.display()))?;
    if !parent.is_dir() {
        return Err(format!(
            "{}: directory {} does not exist",
            path.display(),
            parent.display()
        )
        .into());
    }
    let mut temp = NamedTempFile::new_in(parent).map_err(|e| at(parent, &e))?;
    temp.write_all(content.as_bytes())
        .map_err(|e| at(temp.path(), &e))?;
    match fs::metadata(&target) {
        Ok(existing) => {
            keep_mode_and_owner(temp.as_file(), &existing).map_err(|e| at(&target, &e))?;
        }
        Err(error) if error.kind() == ErrorKind::NotFound => temp
            .as_file()
            .set_permissions(Permissions::from_mode(NEW_FILE_MODE))
            .map_err(|e| at(temp.path(), &e))?,
        Err(error) => return Err(at(&target, &error)),
    }
    temp.as_file().sync_all().map_err(|e| at(temp.path(), &e))?;
    temp.persist(&target).map_err(|e| at(&target, &e.error))?;
    Ok(())
}

fn keep_mode_and_owner(temp: &fs::File, existing: &fs::Metadata) -> io::Result<()> {
    let created = temp.metadata()?;
    // Skipped when equal, so a non-root agent never needs a chown it is not allowed to make.
    if (created.uid(), created.gid()) != (existing.uid(), existing.gid()) {
        fchown(temp, Some(existing.uid()), Some(existing.gid()))?;
    }
    // After the chown, which clears setuid and setgid even for root.
    temp.set_permissions(existing.permissions())
}

fn delete(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != ErrorKind::NotFound => Err(at(path, &error)),
        _ => Ok(()),
    }
}

fn at(path: &Path, error: &io::Error) -> Box<dyn std::error::Error + Send + Sync> {
    format!("{}: {error}", path.display()).into()
}
