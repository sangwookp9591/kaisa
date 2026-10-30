//! The verified path of `kaisa "<goal>"` (T22b, T27c, T31): a developer attempt, trusted checks,
//! a reviewer attempt and the delivery gate; Claude Code's models are chosen by the router.

use std::path::{Path, PathBuf};

use kaisa_agents::{ClaudeLaunch, claude_brokered_argv};
use kaisa_context::{Confirmation, DetectedCommand, Language, Purpose, derive_profile};
use kaisa_daemon::{DEFAULT_REVISION_LIMIT, ROUTE_EFFORT, decide_route};
use kaisa_domain::{
    AgentEntry, AgentKind, Catalog, CatalogModel, Grade, PlannedTask, Role, TaskId, TaskSignals,
    WriteSet, grade_task,
};
use kaisa_sys::ProcessSpec;

/// The model Claude Code runs when the catalog offers nothing for a grade.
const FALLBACK_CLAUDE_MODEL: &str = "sonnet";

/// Claude Code's built-in models and the grades they serve.
fn claude_catalog() -> Catalog {
    let mut catalog = Catalog::new();
    let built_in = AgentEntry {
        name: "claude".to_owned(),
        kind: AgentKind::CliAgent {
            program: "claude".to_owned(),
        },
        models: vec![
            CatalogModel {
                id: "haiku".to_owned(),
                grade: Grade::T1,
            },
            CatalogModel {
                id: "sonnet".to_owned(),
                grade: Grade::T2,
            },
            CatalogModel {
                id: "opus".to_owned(),
                grade: Grade::T3,
            },
        ],
        personal: false,
    };
    // The built-in entry is valid by construction; an empty catalog falls back below.
    let _ = catalog.add(built_in);
    catalog
}

/// The model the router picks for `role` working on `write_set`.
pub(crate) fn routed_model(role: Role, write_set: &WriteSet) -> String {
    let Ok(id) = TaskId::try_new("task_goal") else {
        return FALLBACK_CLAUDE_MODEL.to_owned();
    };
    let task = PlannedTask {
        id,
        role,
        write_set: write_set.clone(),
        depends_on: Vec::new(),
    };
    let signals = TaskSignals {
        paths_in_write_set: write_set.entries().len(),
        impacted_files: write_set.entries().len(),
        touches_security_paths: false,
        is_revision: false,
        prior_failures: 0,
    };
    decide_route(&claude_catalog(), &grade_task(&task, &signals))
        .map_or_else(|| FALLBACK_CLAUDE_MODEL.to_owned(), |route| route.model)
}

/// Claude Code in brokered mode on `model`.
pub(crate) fn claude_spec(model: &str, mcp_config: &Path) -> ProcessSpec {
    ProcessSpec {
        program: PathBuf::from("claude"),
        args: claude_brokered_argv(&ClaudeLaunch {
            model,
            effort: ROUTE_EFFORT,
            mcp_config,
        }),
        working_directory: std::env::temp_dir(),
        env: Vec::new(),
    }
}

/// The trusted checks: the user's own `--check-program` if given, else the test commands the
/// base revision's profile names. Never anything a candidate wrote.
pub(crate) fn trusted_checks(
    repository: &Path,
    user_check: Option<&[String]>,
) -> Vec<DetectedCommand> {
    if let Some(argv) = user_check {
        return vec![DetectedCommand {
            language: Language::Rust,
            purpose: Purpose::Test,
            argv: argv.to_vec(),
            directory: None,
            confirmation: Confirmation::Confirmed,
        }];
    }
    derive_profile(repository)
        .map(|profile| {
            profile
                .commands
                .into_iter()
                .filter(|command| command.purpose == Purpose::Test)
                .collect()
        })
        .unwrap_or_default()
}

/// Revisions allowed before a person decides.
pub(crate) const REVISION_LIMIT: u32 = DEFAULT_REVISION_LIMIT;
