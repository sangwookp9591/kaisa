//! Paths inside a repository, as write sets and the broker use them (architecture §5.2).
//!
//! A `RepoPath` is relative to the repository root, uses `/` on every OS and can never leave
//! the repository. Windows separators are accepted and normalised, so a write set written on
//! one OS means the same files on another.

use std::fmt;

/// Why a string is not a valid repository path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RepoPathError {
    /// The path is empty, or consists only of `.` segments.
    Empty,
    /// The path is absolute (`/x`, `C:\x`, `\\server\x`), so it is not inside the repository.
    Absolute,
    /// A `..` segment would climb out of the repository.
    ClimbsOut,
    /// A Windows spelling aliases a protected Git control path.
    GitControlAlias,
}

impl fmt::Display for RepoPathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("a repository path cannot be empty"),
            Self::Absolute => f.write_str("a repository path must be relative to the repository"),
            Self::GitControlAlias => f.write_str("a Windows path alias names a Git control file"),
            Self::ClimbsOut => f.write_str("a repository path cannot contain `..`"),
        }
    }
}

impl std::error::Error for RepoPathError {}

/// A normalised, repository-relative path such as `src/auth/session.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RepoPath(String);

impl RepoPath {
    /// Parses `text`, accepting `/` or `\` as separators and dropping `.` and empty segments.
    ///
    /// # Errors
    ///
    /// Returns [`RepoPathError`] if the path is empty, absolute or contains `..`.
    pub fn try_new(text: &str) -> Result<Self, RepoPathError> {
        if is_absolute(text) {
            return Err(RepoPathError::Absolute);
        }
        let mut segments: Vec<&str> = Vec::new();
        for segment in text.split(['/', '\\']) {
            match segment {
                "" | "." => {}
                ".." => return Err(RepoPathError::ClimbsOut),
                other => {
                    if windows_git_alias(other) {
                        return Err(RepoPathError::GitControlAlias);
                    }
                    segments.push(other);
                }
            }
        }
        if segments.is_empty() {
            return Err(RepoPathError::Empty);
        }
        Ok(Self(segments.join("/")))
    }

    /// The normalised text, always with `/` separators.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The path's segments, from the repository root down.
    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }
}

impl fmt::Display for RepoPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Keep canonical Git names available for reads; the broker separately denies writes.
/// Reject alternate Windows spellings on every OS so portable policy cannot be bypassed.
fn windows_git_alias(component: &str) -> bool {
    let base = component
        .split(':')
        .next()
        .unwrap_or(component)
        .trim_end_matches(['.', ' ']);
    let short_name = base.to_ascii_uppercase();
    let short_git = ["GIT~", "GITMOD~", "GITATT~"].iter().any(|prefix| {
        short_name.strip_prefix(prefix).is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        })
    });
    short_git
        || (base != component
            && [".git", ".gitattributes", ".gitmodules"]
                .iter()
                .any(|name| base.eq_ignore_ascii_case(name)))
}

/// True for Unix roots, Windows drive paths (`C:`) and UNC or root-relative Windows paths.
fn is_absolute(text: &str) -> bool {
    let bytes: &[u8] = text.as_bytes();
    let starts_at_a_root: bool = matches!(bytes.first(), Some(b'/' | b'\\'));
    let has_a_drive_letter: bool =
        bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    starts_at_a_root || has_a_drive_letter
}

#[cfg(test)]
mod tests {
    use super::{RepoPath, RepoPathError};

    /// A source file an agent typically changes.
    const SESSION_SOURCE: &str = "src/auth/session.rs";

    /// Windows resolves each of these spellings to Git metadata.
    fn windows_git_control_aliases() -> Vec<&'static str> {
        vec![
            ".git.",
            ".git ",
            ".GiT. ",
            "GIT~1",
            "git~123",
            "GITMOD~1",
            "gitatt~123",
            "src/GitMod~2",
            "src/GITATT~9:stream",
            ".git::$INDEX_ALLOCATION",
            ".git:$DATA",
            "src/.gitattributes. ",
            "src/.gitmodules:stream",
            "src/GIT~2/config",
        ]
    }

    #[test]
    fn windows_aliases_of_git_controls_are_rejected_on_every_os() {
        for alias in windows_git_control_aliases() {
            assert_eq!(
                RepoPath::try_new(alias),
                Err(RepoPathError::GitControlAlias),
                "Windows-equivalent control path {alias:?} must not enter a portable write set"
            );
        }
    }

    #[test]
    fn ordinary_unix_colons_dots_and_spaces_remain_valid_paths() {
        for ordinary in [
            "source:stream",
            "file.",
            "file ",
            "git~source",
            ".github",
            ".gitignore",
        ] {
            assert!(
                RepoPath::try_new(ordinary).is_ok(),
                "ordinary Unix name {ordinary:?} must remain available"
            );
        }
    }

    #[test]
    fn a_relative_path_inside_the_repository_is_accepted() {
        let path: RepoPath = RepoPath::try_new(SESSION_SOURCE).expect("a plain relative path");

        assert_eq!(
            path.as_str(),
            SESSION_SOURCE,
            "a path that is already normal is kept exactly"
        );
    }

    #[test]
    fn a_path_that_climbs_out_of_the_repository_is_rejected() {
        assert_eq!(
            RepoPath::try_new("src/../../etc/passwd"),
            Err(RepoPathError::ClimbsOut),
            "a write set must never reach files outside the repository"
        );
    }

    #[test]
    fn an_absolute_path_is_rejected() {
        for absolute in [
            "/etc/passwd",
            r"C:\Windows\system32",
            r"\\server\share\file",
        ] {
            assert_eq!(
                RepoPath::try_new(absolute),
                Err(RepoPathError::Absolute),
                "{absolute} names a place outside any repository"
            );
        }
    }

    #[test]
    fn windows_separators_are_normalised_to_forward_slashes() {
        let typed_on_windows: RepoPath =
            RepoPath::try_new(r"src\auth\.\session.rs").expect("a relative Windows path");

        assert_eq!(
            typed_on_windows.as_str(),
            SESSION_SOURCE,
            "the same file must have the same name on every OS"
        );
    }
}
