use serde::{Deserialize, Serialize};

/// A host's desired resources, in declaration order.
pub type Catalog = Vec<Resource>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Pkg,
    File,
    Service,
    User,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceKey {
    pub kind: Kind,
    pub id: String,
}

/// A catalog entry: `{"id": ..., "kind": ..., "props": ...}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resource {
    pub id: String,
    #[serde(flatten)]
    pub desired: Desired,
}

impl Resource {
    #[must_use]
    pub fn key(&self) -> ResourceKey {
        ResourceKey {
            kind: self.desired.kind(),
            id: self.id.clone(),
        }
    }
}

/// Desired state of one resource; `props` is the second argument of its `fettle.ts` call.
///
/// Validate a declaration by deserializing `{"kind": ..., "props": ...}` into this type
/// rather than into [`Resource`], whose `flatten` hides the path to a failing prop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "props", rename_all = "lowercase")]
pub enum Desired {
    Pkg(Pkg),
    File(File),
    Service(Service),
    User(User),
}

impl Desired {
    #[must_use]
    pub fn kind(&self) -> Kind {
        match self {
            Self::Pkg(_) => Kind::Pkg,
            Self::File(_) => Kind::File,
            Self::Service(_) => Kind::Service,
            Self::User(_) => Kind::User,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Pkg {
    #[serde(default)]
    pub absent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "FileProps", into = "FileProps")]
pub enum File {
    Present { content: String },
    Absent,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct FileProps {
    content: Option<String>,
    #[serde(default)]
    absent: bool,
}

impl TryFrom<FileProps> for File {
    type Error = &'static str;

    fn try_from(props: FileProps) -> Result<Self, Self::Error> {
        match (props.absent, props.content) {
            (true, None) => Ok(Self::Absent),
            (true, Some(_)) => Err("`absent: true` excludes `content`"),
            (false, Some(content)) => Ok(Self::Present { content }),
            (false, None) => Err("missing field `content`, required unless `absent` is true"),
        }
    }
}

impl From<File> for FileProps {
    fn from(file: File) -> Self {
        match file {
            File::Present { content } => Self {
                content: Some(content),
                absent: false,
            },
            File::Absent => Self {
                content: None,
                absent: true,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Service {
    pub enabled: Option<bool>,
}

/// An omitted prop is `None`, distinct from an empty list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct User {
    pub groups: Option<Vec<String>>,
    pub ssh_keys: Option<Vec<String>>,
}
