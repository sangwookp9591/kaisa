//! The project profile: languages and the commands that build, test and lint a repository,
//! derived by Kaisa with no setup (architecture §5.9, PRD FR-22).

use kaisa_domain::RepoPath;

/// A language Kaisa can derive a profile for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Language {
    /// Cargo projects and workspaces.
    Rust,
    /// Projects with a `package.json`.
    Node,
    /// Projects with a `pyproject.toml`.
    Python,
    /// Modules with a `go.mod`.
    Go,
}

/// What a command is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Purpose {
    /// Compiles the project.
    Build,
    /// Runs the project's tests.
    Test,
    /// Runs the project's linters.
    Lint,
}

/// Whether Kaisa has seen a command work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirmation {
    /// Found in the project's files but not run yet.
    Detected,
    /// Ran in this repository and exited successfully.
    Confirmed,
    /// Ran in this repository and failed; it is kept, but checks cannot rely on it.
    Failed,
}

/// A command found in the project, as an argument vector. It is never a shell string, so no
/// project file can smuggle shell syntax into what Kaisa runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedCommand {
    /// The language whose files defined the command.
    pub language: Language,
    /// What the command is for.
    pub purpose: Purpose,
    /// Program and arguments.
    pub argv: Vec<String>,
    /// Where to run it, relative to the repository root; `None` for the root itself.
    pub directory: Option<RepoPath>,
    /// Whether it has been seen to work.
    pub confirmation: Confirmation,
}

/// The beginning of a repository instruction file, kept as quoted data for context packs. Its
/// text is never parsed for commands: what an instruction file says to run is not what Kaisa runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionExcerpt {
    /// The file, relative to the repository root.
    pub path: RepoPath,
    /// The first part of the file, cut on a character boundary.
    pub text: String,
    /// True if the file was longer than the excerpt.
    pub truncated: bool,
}

/// Everything Kaisa derived about a repository at one base revision.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProjectProfile {
    /// Languages found, in a stable order.
    pub languages: Vec<Language>,
    /// Commands found, per language and purpose.
    pub commands: Vec<DetectedCommand>,
    /// Excerpts of the repository's instruction files, as untrusted, quoted data.
    pub instructions: Vec<InstructionExcerpt>,
}
