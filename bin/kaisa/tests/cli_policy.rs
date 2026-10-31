//! `kaisa policy` as a user's terminal sees it, against a policy file written by an older release.

use std::path::PathBuf;
use std::process::{Command, Output};

/// The rules a safe policy keeps true, one of which an older release let a person switch off.
const GATE_KEYS: [&str; 2] = [
    "require_person_when_tests_weaken",
    "require_person_when_check_definitions_change",
];

struct State(PathBuf);

impl State {
    fn with_older_policy_switching_off(key: &str) -> Self {
        let directory =
            std::env::temp_dir().join(format!("kaisa-cli-policy-{key}-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("writable temp dir");
        let mut policy = serde_json::json!({
            "revision_limit": 2, "allowed_egress_hosts": [], "confine_agents": true,
            "min_grade_for_security_review": "T3",
            "require_person_when_check_definitions_change": true,
            "require_person_when_tests_weaken": true,
        });
        policy[key] = serde_json::json!(false);
        std::fs::write(directory.join("policy.json"), policy.to_string()).expect("older policy");
        Self(directory)
    }

    fn kaisa(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_kaisa"))
            .args(args)
            .env("KAISA_STATE_DIR", &self.0)
            .output()
            .expect("kaisa runs")
    }
}

impl Drop for State {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn an_older_policy_with_a_gate_switched_off_is_explained_on_stderr_for_show_and_set() {
    for key in GATE_KEYS {
        let state = State::with_older_policy_switching_off(key);
        for args in [
            vec!["policy", "show"],
            vec!["policy", "set", "revision_limit", "3"],
        ] {
            let run = state.kaisa(&args);
            let stderr = String::from_utf8_lossy(&run.stderr);
            assert_eq!(run.status.code(), Some(1), "{args:?} refuses the old file");
            assert!(run.stdout.is_empty(), "no policy is printed for {args:?}");
            assert!(
                stderr.starts_with("kaisa: ")
                    && stderr.contains(key)
                    && stderr.contains("policy.json")
                    && stderr.contains("repair:"),
                "{args:?} names {key}, the file and how to repair it, got {stderr:?}"
            );
        }
    }
}
