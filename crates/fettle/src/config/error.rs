use std::fmt;
use std::path::{Path, PathBuf};

/// A position in `fettle.ts`, one-based.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub file: PathBuf,
    pub line: u32,
    pub column: u32,
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.file.display(), self.line, self.column)
    }
}

/// An evaluation failure; `location` is `None` only when no line of `fettle.ts` caused it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub location: Option<Location>,
    pub message: String,
}

impl Error {
    pub(crate) fn new(location: Option<Location>, message: impl Into<String>) -> Self {
        Self {
            location,
            message: message.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.location {
            Some(location) => write!(f, "{location}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for Error {}

/// Maps positions in the stripped JavaScript back to `fettle.ts`.
#[derive(Debug, Default)]
pub(crate) struct SourceMap {
    /// `(generated line, generated column, original line, original column)`, zero-based,
    /// sorted by generated position as oxc emits them.
    tokens: Vec<(u32, u32, u32, u32)>,
}

impl SourceMap {
    pub(crate) fn new(tokens: Vec<(u32, u32, u32, u32)>) -> Self {
        Self { tokens }
    }

    /// Maps a one-based generated position to a one-based original one. A column before the
    /// line's first token maps to that token, since `QuickJS` columns are best effort.
    fn original(&self, line: u32, column: u32) -> Option<(u32, u32)> {
        let (line, column) = (line.checked_sub(1)?, column.saturating_sub(1));
        let start = self.tokens.partition_point(|t| t.0 < line);
        let end = self.tokens.partition_point(|t| t.0 <= line);
        let on_line = &self.tokens[start..end];
        let before = on_line.partition_point(|t| t.1 <= column);
        let token = on_line.get(before.saturating_sub(1))?;
        Some((token.2 + 1, token.3 + 1))
    }
}

/// Finds the first frame of `stack` in `module` and maps it to its place in `fettle.ts`.
pub(crate) fn locate(stack: &str, module: &str, map: &SourceMap, file: &Path) -> Option<Location> {
    let (line, column) = stack
        .lines()
        .filter_map(frame)
        .find(|(name, _, _)| *name == module)
        .map(|(_, line, column)| (line, column))?;
    let (line, column) = map.original(line, column)?;
    Some(Location {
        file: file.to_path_buf(),
        line,
        column,
    })
}

/// Parses `at name (file:line:column)` or `at file:line:column`.
fn frame(line: &str) -> Option<(&str, u32, u32)> {
    let rest = line.trim().strip_prefix("at ")?;
    let position = match rest.rfind('(') {
        Some(open) if rest.ends_with(')') => &rest[open + 1..rest.len() - 1],
        _ => rest,
    };
    let mut parts = position.rsplitn(3, ':');
    let column = parts.next()?.parse().ok()?;
    let line = parts.next()?.parse().ok()?;
    Some((parts.next()?, line, column))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODULE: &str = "/abs/fettle.ts";

    fn identity() -> SourceMap {
        SourceMap::new((0..20).map(|line| (line, 0, line, 0)).collect())
    }

    fn line_of(stack: &str) -> Option<u32> {
        locate(stack, MODULE, &identity(), Path::new("fettle.ts")).map(|l| l.line)
    }

    #[test]
    fn parses_named_and_anonymous_frames() {
        assert_eq!(
            frame("    at role (/abs/fettle.ts:3:2)"),
            Some((MODULE, 3, 2))
        );
        assert_eq!(frame("    at /abs/fettle.ts:7:9"), Some((MODULE, 7, 9)));
        assert_eq!(
            frame("    at <anonymous> (fettle:12:5)"),
            Some(("fettle", 12, 5))
        );
        assert_eq!(frame("Error: boom"), None);
    }

    #[test]
    fn skips_frames_from_fettle() {
        let stack = "    at stack (fettle:4:10)\n    at <anonymous> (fettle:15:34)\n    at role (/abs/fettle.ts:3:2)\n    at /abs/fettle.ts:9:1\n";
        assert_eq!(line_of(stack), Some(3));
    }

    #[test]
    fn stack_without_module_frame_has_no_location() {
        assert_eq!(line_of("    at stack (fettle:4:10)\n"), None);
    }

    #[test]
    fn maps_generated_lines_to_original_ones() {
        let map = SourceMap::new(vec![(0, 0, 4, 0), (2, 0, 9, 0), (2, 6, 9, 8)]);
        assert_eq!(map.original(1, 1), Some((5, 1)));
        assert_eq!(map.original(3, 8), Some((10, 9)));
        assert_eq!(map.original(3, 1), Some((10, 1)));
        assert_eq!(map.original(2, 1), None);
    }
}
