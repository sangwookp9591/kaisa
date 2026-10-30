//! A reviewer's verdict on a frozen candidate (T21c, ADR-0030).

use crate::repo_path::RepoPath;

/// How much a finding matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Must be fixed before delivery; starts a revision.
    Blocking,
    /// Worth knowing; never stops delivery.
    Advisory,
}

impl Severity {
    /// The severity named by `code` in a verdict.
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "blocking" => Some(Self::Blocking),
            "advisory" => Some(Self::Advisory),
            _ => None,
        }
    }
}

/// One thing a reviewer found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// How much it matters.
    pub severity: Severity,
    /// The file it is about.
    pub path: RepoPath,
    /// The line, if the reviewer gave one.
    pub line: Option<u32>,
    /// What is wrong, in the reviewer's words (untrusted text).
    pub message: String,
}

/// A validated verdict, bound to the candidate tree it was given on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewVerdict {
    /// The candidate tree reviewed.
    pub candidate_tree: String,
    /// What the reviewer found.
    pub findings: Vec<Finding>,
}

impl ReviewVerdict {
    /// True if nothing blocks delivery of `tree`: the verdict is for that exact tree and has
    /// no blocking finding.
    #[must_use]
    pub fn approves(&self, tree: &str) -> bool {
        self.candidate_tree == tree
            && !self
                .findings
                .iter()
                .any(|finding| finding.severity == Severity::Blocking)
    }

    /// The findings a revision must fix.
    #[must_use]
    pub fn blocking(&self) -> Vec<&Finding> {
        self.findings
            .iter()
            .filter(|finding| finding.severity == Severity::Blocking)
            .collect()
    }
}
