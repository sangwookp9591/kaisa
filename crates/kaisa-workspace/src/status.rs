//! Parsing `git status --porcelain=v2 -z` into typed entries.
//!
//! Records are NUL-separated. A rename or copy record is followed by an extra record holding
//! the original path, which is consumed and not reported separately.

use std::fmt;

/// The record separator used by `git status -z`.
const RECORD_SEPARATOR: u8 = 0;

/// Untracked (`? path`) and ignored (`! path`) entries carry the path right after the marker.
const NO_FIELDS_BEFORE_PATH: usize = 0;

/// Fields before the path in an ordinary changed entry (`1 XY sub mH mI mW hH hI path`).
const ORDINARY_FIELDS_BEFORE_PATH: usize = 8;

/// Fields before the path in a renamed or copied entry (`2 XY sub mH mI mW hH hI Xscore path`).
const RENAMED_FIELDS_BEFORE_PATH: usize = 9;

/// Fields before the path in an unmerged entry (`u XY sub m1 m2 m3 mW h1 h2 h3 path`).
const UNMERGED_FIELDS_BEFORE_PATH: usize = 10;

/// What git says about one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EntryKind {
    /// A tracked file with staged or unstaged changes (including renames and copies).
    Tracked,
    /// A file git does not track.
    Untracked,
    /// A file matched by an ignore rule.
    Ignored,
    /// A file with unresolved merge conflicts.
    Conflicted,
}

/// One path reported by `git status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    /// Path relative to the worktree root, as git reports it.
    pub path: String,
    /// What kind of change it is.
    pub kind: EntryKind,
}

/// A record that does not follow the porcelain v2 format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusParseError {
    /// The offending record, lossily decoded for the error message.
    pub record: String,
}

impl fmt::Display for StatusParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unexpected `git status` record: {:?}", self.record)
    }
}

impl std::error::Error for StatusParseError {}

/// Parses the full output of `git status --porcelain=v2 -z`.
///
/// # Errors
///
/// Returns [`StatusParseError`] for the first record that does not follow the format.
pub fn parse_status(output: &[u8]) -> Result<Vec<StatusEntry>, StatusParseError> {
    let mut records = output
        .split(|byte| *byte == RECORD_SEPARATOR)
        .filter(|record| !record.is_empty());
    let mut entries: Vec<StatusEntry> = Vec::new();
    while let Some(record) = records.next() {
        let text: String = String::from_utf8_lossy(record).into_owned();
        let malformed = || StatusParseError {
            record: text.clone(),
        };
        let (marker, rest): (&str, &str) = text.split_once(' ').ok_or_else(malformed)?;
        let (kind, fields_before_path): (EntryKind, usize) = match marker {
            "#" => continue,
            "1" => (EntryKind::Tracked, ORDINARY_FIELDS_BEFORE_PATH),
            "2" => (EntryKind::Tracked, RENAMED_FIELDS_BEFORE_PATH),
            "u" => (EntryKind::Conflicted, UNMERGED_FIELDS_BEFORE_PATH),
            "?" => (EntryKind::Untracked, NO_FIELDS_BEFORE_PATH),
            "!" => (EntryKind::Ignored, NO_FIELDS_BEFORE_PATH),
            _ => return Err(malformed()),
        };
        let path: &str = if fields_before_path == NO_FIELDS_BEFORE_PATH {
            rest
        } else {
            text.splitn(fields_before_path + 1, ' ')
                .nth(fields_before_path)
                .ok_or_else(malformed)?
        };
        if path.is_empty() {
            return Err(malformed());
        }
        if marker == "2" {
            // The original path of a rename or copy follows as its own record.
            records.next().ok_or_else(malformed)?;
        }
        entries.push(StatusEntry {
            path: path.to_owned(),
            kind,
        });
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::{EntryKind, StatusEntry, StatusParseError, parse_status};

    /// What `git status` prints for a modified tracked file.
    fn git_reports_modified(path: &str) -> String {
        format!("1 .M N... 100644 100644 100644 3f2a1b0 3f2a1b0 {path}\0")
    }

    /// What `git status` prints for a file renamed from `from` to `to`.
    fn git_reports_renamed(from: &str, to: &str) -> String {
        format!("2 R. N... 100644 100644 100644 3f2a1b0 3f2a1b0 R100 {to}\0{from}\0")
    }

    /// What `git status` prints for an untracked file.
    fn git_reports_untracked(path: &str) -> String {
        format!("? {path}\0")
    }

    /// What `git status` prints for an ignored file.
    fn git_reports_ignored(path: &str) -> String {
        format!("! {path}\0")
    }

    /// What `git status` prints for a file with merge conflicts.
    fn git_reports_conflicted(path: &str) -> String {
        format!("u UU N... 100644 100644 100644 100644 3f2a1b0 4b3c2d1 5c4d3e2 {path}\0")
    }

    /// The branch header lines `git status --branch` prints before any entry.
    fn git_reports_branch_headers() -> String {
        "# branch.oid 3f2a1b0\0# branch.head main\0".to_owned()
    }

    fn parsed(output: &str) -> Vec<StatusEntry> {
        parse_status(output.as_bytes()).expect("well-formed git output parses")
    }

    fn entry(path: &str, kind: EntryKind) -> StatusEntry {
        StatusEntry {
            path: path.to_owned(),
            kind,
        }
    }

    #[test]
    fn an_ordinary_changed_entry_is_parsed_as_tracked() {
        assert_eq!(
            parsed(&git_reports_modified("src/login.rs")),
            [entry("src/login.rs", EntryKind::Tracked)],
            "a modified file is a tracked change"
        );
    }

    #[test]
    fn an_untracked_entry_is_parsed_as_untracked() {
        assert_eq!(
            parsed(&git_reports_untracked("notes.md")),
            [entry("notes.md", EntryKind::Untracked)],
            "a new file git does not know is untracked"
        );
    }

    #[test]
    fn an_ignored_entry_is_parsed_as_ignored() {
        assert_eq!(
            parsed(&git_reports_ignored("target/")),
            [entry("target/", EntryKind::Ignored)],
            "a path matched by .gitignore is ignored"
        );
    }

    #[test]
    fn a_conflicted_entry_is_parsed_as_conflicted() {
        assert_eq!(
            parsed(&git_reports_conflicted("src/session.rs")),
            [entry("src/session.rs", EntryKind::Conflicted)],
            "unresolved merges are their own kind, never treated as ordinary changes"
        );
    }

    #[test]
    fn a_renamed_entry_is_parsed_as_tracked_under_its_new_path() {
        assert_eq!(
            parsed(&git_reports_renamed("src/auth.rs", "src/login.rs")),
            [entry("src/login.rs", EntryKind::Tracked)],
            "the original path record is consumed, not reported as a second entry"
        );
    }

    #[test]
    fn a_path_with_spaces_is_kept_whole() {
        assert_eq!(
            parsed(&git_reports_modified("docs/release notes.md")),
            [entry("docs/release notes.md", EntryKind::Tracked)],
            "spaces belong to the path; only the leading fields are split off"
        );
    }

    #[test]
    fn branch_header_lines_are_skipped() {
        let output: String = git_reports_branch_headers() + &git_reports_untracked("notes.md");

        assert_eq!(
            parsed(&output),
            [entry("notes.md", EntryKind::Untracked)],
            "headers describe the branch, not files"
        );
    }

    #[test]
    fn a_malformed_record_is_reported_with_its_text() {
        let garbage: &str = "this is not git output\0";

        assert_eq!(
            parse_status(garbage.as_bytes()),
            Err(StatusParseError {
                record: "this is not git output".to_owned()
            }),
            "unexpected input must be surfaced with the text that caused it"
        );
    }
}
