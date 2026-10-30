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

/// A tool-less local model used only for advisory first-pass review.
pub(crate) struct LocalRoute {
    /// The registered OpenAI-compatible endpoint.
    pub(crate) endpoint: String,
    /// The registered T0 model identifier.
    pub(crate) model: String,
}

/// The cheapest registered local model for first-pass review, or no first pass.
///
/// Only T0 models qualify; equal grades retain registration order within and across agents.
pub(crate) fn first_pass_route(personal: &Catalog) -> Option<LocalRoute> {
    for entry in personal.entries() {
        let AgentKind::OpenAiCompatible { endpoint } = &entry.kind else {
            continue;
        };
        if let Some(model) = entry.models.iter().find(|model| model.grade == Grade::T0) {
            return Some(LocalRoute {
                endpoint: endpoint.clone(),
                model: model.id.clone(),
            });
        }
    }
    None
}

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

/// The Claude Code model for `role` working on `write_set`; tool-less models are excluded.
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

/// The policy a run adopts until personal policy files arrive: the default revision limit,
/// confinement on, no egress, a T3 floor for security review, and a person for changed checks.
pub(crate) fn default_policy() -> kaisa_domain::Policy {
    kaisa_domain::Policy {
        revision_limit: REVISION_LIMIT,
        allowed_egress_hosts: Vec::new(),
        confine_agents: true,
        min_grade_for_security_review: kaisa_domain::Grade::T3,
        require_person_when_check_definitions_change: true,
    }
}

#[cfg(test)]
mod tests {
    use kaisa_domain::{
        AgentEntry, AgentKind, Catalog, CatalogModel, Grade, RepoPath, Role, WriteSet,
    };

    use super::{first_pass_route, routed_model};

    /// The endpoint of the first registered local agent.
    const LOCAL_ENDPOINT: &str = "http://localhost:11434/v1";
    /// The endpoint of a later registration with the same grade.
    const LATER_ENDPOINT: &str = "http://127.0.0.1:8080/v1";
    /// The first registered model wins over alphabetically earlier model identifiers.
    const FIRST_MODEL: &str = "z-local-first";
    /// A later model with the same grade cannot displace the first model.
    const LATER_MODEL: &str = "a-local-later";
    /// The path owned by a code-editing task.
    const CODE_PATH: &str = "src/lib.rs";
    /// Claude Code's model for a small code edit.
    const CODE_MODEL: &str = "haiku";
    /// Claude Code's model for security review.
    const SECURITY_MODEL: &str = "opus";

    /// A personal registration offering the given model grade.
    fn local_agent(name: &str, grade: Grade) -> AgentEntry {
        AgentEntry {
            name: name.to_owned(),
            kind: AgentKind::OpenAiCompatible {
                endpoint: LOCAL_ENDPOINT.to_owned(),
            },
            models: vec![CatalogModel {
                id: FIRST_MODEL.to_owned(),
                grade,
            }],
            personal: true,
        }
    }

    /// A catalog retaining the order of valid fixture registrations.
    fn catalog_with(entries: Vec<AgentEntry>) -> Catalog {
        let mut catalog = Catalog::new();
        for entry in entries {
            catalog
                .add(entry)
                .expect("the personal registration is valid");
        }
        catalog
    }

    #[test]
    fn a_registered_local_model_takes_first_pass_review_and_never_code_edits() {
        let mut first = local_agent("z-first", Grade::T0);
        first.models.insert(
            0,
            CatalogModel {
                id: "standard".to_owned(),
                grade: Grade::T1,
            },
        );
        first.models.push(CatalogModel {
            id: LATER_MODEL.to_owned(),
            grade: Grade::T0,
        });
        let mut later = local_agent("a-later", Grade::T0);
        later.kind = AgentKind::OpenAiCompatible {
            endpoint: LATER_ENDPOINT.to_owned(),
        };
        later.models[0].id = LATER_MODEL.to_owned();
        let personal = catalog_with(vec![first, later]);
        let route = first_pass_route(&personal).expect("a registered T0 model takes first pass");
        let write_set = WriteSet::new(vec![
            RepoPath::try_new(CODE_PATH).expect("a valid code path"),
        ]);

        assert_eq!(
            (route.endpoint.as_str(), route.model.as_str()),
            (LOCAL_ENDPOINT, FIRST_MODEL),
            "first-pass review picks T0 and preserves registration order across agents and models"
        );
        assert_eq!(
            routed_model(Role::Developer, &write_set),
            CODE_MODEL,
            "code edits retain a Claude Code model even when a tool-less T0 model is registered"
        );
        assert_eq!(
            routed_model(Role::Security, &write_set),
            SECURITY_MODEL,
            "security review retains the T3 Claude Code model rather than the local first pass"
        );
    }

    #[test]
    fn no_first_pass_runs_without_a_t0_local_model() {
        assert!(
            first_pass_route(&Catalog::new()).is_none(),
            "an empty personal catalog does not add a first-pass review"
        );
    }

    #[test]
    fn a_t1_local_model_is_not_light_work() {
        for grade in [Grade::T1, Grade::T2, Grade::T3] {
            let personal = catalog_with(vec![local_agent("local", grade)]);

            assert!(
                first_pass_route(&personal).is_none(),
                "a {grade:?} local model cannot substitute for the exact T0 light-work grade"
            );
        }
    }

    #[test]
    fn a_cli_agent_is_never_a_first_pass_route() {
        let mut cli = local_agent("local-cli", Grade::T0);
        cli.kind = AgentKind::CliAgent {
            program: "local-cli".to_owned(),
        };
        let personal = catalog_with(vec![cli.clone()]);

        assert!(
            first_pass_route(&personal).is_none(),
            "even a T0 CLI agent cannot take the tool-less first-pass route"
        );

        let personal = catalog_with(vec![cli, local_agent("local", Grade::T0)]);
        let route = first_pass_route(&personal).expect("the local T0 model remains eligible");

        assert_eq!(
            route.model, FIRST_MODEL,
            "a CLI registration is skipped so the later local T0 model takes first-pass review"
        );
    }
}
