//! Read-only staged review with a real scripted process and isolated state; no model calls.

use std::path::{Path, PathBuf};

use kaisa_daemon::staged_review::{STAGES, StagedReviewInput, context_bundle, run_staged_review};
use kaisa_daemon::{GoalRequest, prepare_review_repository};
use kaisa_domain::{
    ContainmentCapability, ReducedModeBasis, ReducedModeDecision, RepoPath, Role, WriteSet,
};
use kaisa_sys::{ProcessExit, ProcessSpec};
use serde_json::json;

const REVIEW_ONLY: &str = "REVIEW_ONLY_DEVELOPER_DECISIONS";
const FORBIDDEN_WRITE: &str = "reviewer-write.md";
const DECISION_MS: u64 = 600_000;
const SUCCESS_EXIT: ProcessExit = ProcessExit::Code(0);

struct Bench(PathBuf);
impl Drop for Bench {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn approval(tree: &str) -> String {
    let answer = json!({"stages":[
        {"stage":STAGES[0],"concerns":[]},
        {"stage":STAGES[1],"assessment":"criteria and tests agree"},
        {"stage":STAGES[2],"assessment":"no unexplained decisions"},
        {"stage":STAGES[3],"architecture":"cohesive","domain_invariants":"preserved","error_handling":"explicit","security":"reviewed","external_integrations":"none"},
        {"stage":STAGES[4],"changes":[]}
    ],"candidate_tree":tree,"approve":true,"findings":[]});
    format!("```kaisa-review\n{answer}\n```")
}

#[test]
fn a_scripted_staged_reviewer_gets_ordered_context_and_its_write_is_refused() {
    let bench =
        Bench(std::env::temp_dir().join(format!("kaisa-staged-review-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&bench.0);
    std::fs::create_dir_all(&bench.0).expect("scratch bench");
    let repository = bench.0.join("review-repository");
    prepare_review_repository(&repository).expect("fresh review repository");
    let state = bench.0.join("state");
    let answer = bench.0.join("answer.txt");
    let context = bench.0.join("context.txt");
    let write_result = bench.0.join("write-result.txt");
    let input = StagedReviewInput {
        candidate_tree: "frozen-tree".into(),
        bounded_diff: "bounded diff".into(),
        tests_and_spec: "AC-1 test passes".into(),
        developer_final_answer: REVIEW_ONLY.into(),
        ..Default::default()
    };
    std::fs::write(&answer, approval(&input.candidate_tree)).expect("scripted approval");
    let launch = |config: &Path| ProcessSpec {
        program: PathBuf::from(env!("CARGO_BIN_EXE_kaisa"))
            .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX)),
        args: vec![
            "--mcp-config".into(),
            config.display().to_string(),
            "--fake-test-review-answer".into(),
            answer.display().to_string(),
            "--fake-dump-context".into(),
            context.display().to_string(),
            "--fake-try-write".into(),
            FORBIDDEN_WRITE.into(),
            "--fake-write-outcome".into(),
            write_result.display().to_string(),
        ],
        working_directory: std::env::temp_dir(),
        env: vec![],
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis();
    let decision = ReducedModeBasis::Decision(ReducedModeDecision {
        accepts_missing: ContainmentCapability::ALL.to_vec(),
        expires_at_ms: u64::try_from(now).expect("timestamp") + DECISION_MS,
    });
    let paths = WriteSet::new(vec![
        RepoPath::try_new(FORBIDDEN_WRITE).expect("valid write probe path"),
    ]);
    let outcome = run_staged_review(
        GoalRequest {
            repository: &repository,
            goal: "replaced",
            base: "HEAD",
            state_directory: &state,
            write_set: &paths,
            role: Role::Developer,
            launch: &launch,
            kaisa_executable: Path::new(env!("CARGO_BIN_EXE_kaisa")),
            confine_agent: false,
            egress_hosts: &[],
            agent_writable: &[],
            local_model_port: None,
            observed: false,
            reduced_mode: &decision,
        },
        &input,
    )
    .expect("scripted review attempt");
    assert_eq!(
        outcome.attempt.exit,
        Some(SUCCESS_EXIT),
        "the scripted process finished"
    );
    assert!(
        outcome
            .review
            .expect("valid review")
            .verdict()
            .approves(&input.candidate_tree),
        "the reviewer returns a validated candidate-bound approval"
    );
    assert_eq!(
        std::fs::read_to_string(write_result).expect("probe result"),
        "refused",
        "reviewer role and empty write set deny writes even when developer settings were supplied"
    );
    assert!(
        outcome.attempt.commit.is_none(),
        "reviewing never creates a candidate write commit"
    );
    let pack = std::fs::read_to_string(context).expect("captured context");
    assert!(
        pack.contains(REVIEW_ONLY),
        "review-only context reaches this reviewer"
    );
    let mut last = None;
    for stage in STAGES {
        // The schema also names the stages, so inspect the actual bundle separately.
        let position = context_bundle(&input)
            .to_string()
            .find(stage)
            .expect("context stage");
        assert!(
            last.is_none_or(|previous| position > previous),
            "the context stages are ordered"
        );
        last = Some(position);
    }
    let public = kaisa_daemon::qa_interface::extract_public_interface(&repository)
        .expect("QA public interface")
        .as_json()
        .to_string();
    assert!(
        !public.contains(REVIEW_ONLY),
        "review context is not persisted where QA public context is built"
    );
    assert!(
        !repository.join(FORBIDDEN_WRITE).exists(),
        "the original repository has no reviewer write"
    );
}
