//! Write sets: the paths a writing Task may change (architecture §5.2, "Write-set ownership").
//!
//! An entry names a file or a whole directory. Two Tasks whose write sets overlap are never in
//! progress together, so overlap is decided on whole path components: `src/auth` overlaps
//! `src/auth/session.rs` but not `src/authz`.

use crate::repo_path::RepoPath;

/// The paths one Task may change.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WriteSet {
    entries: Vec<RepoPath>,
}

impl WriteSet {
    /// A write set made of `entries`, each a file or a directory.
    #[must_use]
    pub fn new(entries: Vec<RepoPath>) -> Self {
        Self { entries }
    }

    /// The entries, in the order they were declared.
    #[must_use]
    pub fn entries(&self) -> &[RepoPath] {
        &self.entries
    }

    /// True if some path could be changed by both write sets.
    #[must_use]
    pub fn overlaps(&self, other: &WriteSet) -> bool {
        self.entries.iter().any(|mine| {
            other
                .entries
                .iter()
                .any(|theirs| is_within(mine, theirs) || is_within(theirs, mine))
        })
    }
}

/// True if `path` is `ancestor` itself or lies below it, comparing whole segments.
pub(crate) fn is_within(path: &RepoPath, ancestor: &RepoPath) -> bool {
    let mut below = path.segments();
    ancestor
        .segments()
        .all(|ancestor_segment| below.next() == Some(ancestor_segment))
}

#[cfg(test)]
mod tests {
    use super::WriteSet;
    use crate::repo_path::RepoPath;

    fn path(text: &str) -> RepoPath {
        RepoPath::try_new(text).expect("test paths are valid repository paths")
    }

    fn a_task_owning(paths: &[&str]) -> WriteSet {
        WriteSet::new(paths.iter().map(|&text| path(text)).collect())
    }

    #[test]
    fn write_sets_on_disjoint_directories_do_not_overlap() {
        let api_task: WriteSet = a_task_owning(&["services/api"]);
        let web_task: WriteSet = a_task_owning(&["services/web"]);

        assert!(
            !api_task.overlaps(&web_task),
            "work on separate areas may run in parallel"
        );
    }

    #[test]
    fn a_directory_overlaps_a_file_inside_it() {
        let auth_refactor: WriteSet = a_task_owning(&["src/auth"]);
        let session_fix: WriteSet = a_task_owning(&["src/auth/session.rs"]);

        assert!(
            auth_refactor.overlaps(&session_fix) && session_fix.overlaps(&auth_refactor),
            "both tasks could change session.rs, so they must not run together"
        );
    }

    #[test]
    fn a_shared_name_prefix_is_not_an_overlap() {
        let authentication: WriteSet = a_task_owning(&["src/auth"]);
        let authorization: WriteSet = a_task_owning(&["src/authz"]);

        assert!(
            !authentication.overlaps(&authorization),
            "paths are compared by whole segments, not by characters"
        );
    }

    #[test]
    fn the_same_file_in_two_write_sets_overlaps() {
        let first: WriteSet = a_task_owning(&["Cargo.toml", "src/lib.rs"]);
        let second: WriteSet = a_task_owning(&["docs", "Cargo.toml"]);

        assert!(
            first.overlaps(&second),
            "two tasks editing the same manifest is the classic merge conflict"
        );
    }

    #[test]
    fn an_empty_write_set_overlaps_nothing() {
        let read_only_review: WriteSet = WriteSet::default();
        let any_change: WriteSet = a_task_owning(&["src"]);

        assert!(
            !read_only_review.overlaps(&any_change),
            "a task that writes nothing never blocks a writer"
        );
    }
}
