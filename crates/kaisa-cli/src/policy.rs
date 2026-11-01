//! `kaisa policy show|set`: personal rules captured by future runs.

use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use kaisa_domain::{Grade, Policy};
use serde_json::{Value, json};

/// The personal policy file within the state directory.
const POLICY_FILE: &str = "policy.json";
/// The largest revision budget accepted by the personal CLI.
const MAX_REVISION_LIMIT: u32 = 10;
/// Distinguishes concurrent temporary files within this process.
static NEXT_TEMPORARY_FILE: AtomicU64 = AtomicU64::new(0);
/// The command surface and accepted values.
const USAGE: &str = "usage: kaisa policy show
       kaisa policy set <key> <value>
keys: allow_provisional_spec (true|false), revision_limit (0..10), confine_agents (true|false),
      allowed_egress_hosts (comma-separated; empty clears),
      require_person_when_check_definitions_change (true only; always enforced),
      require_person_when_tests_weaken (true only; always enforced),
      min_grade_for_security_review (T0..T3; requires T3)";

fn grade_from(code: &str) -> Option<Grade> {
    match code {
        "T0" => Some(Grade::T0),
        "T1" => Some(Grade::T1),
        "T2" => Some(Grade::T2),
        "T3" => Some(Grade::T3),
        _ => None,
    }
}

fn policy_from_json(value: &Value) -> Option<Policy> {
    Some(Policy {
        allow_provisional_spec: match value.get("allow_provisional_spec") {
            None => false,
            Some(value) => value.as_bool()?,
        },
        require_person_when_tests_weaken: match value.get("require_person_when_tests_weaken") {
            None => true,
            Some(value) => value.as_bool()?,
        },
        revision_limit: u32::try_from(value["revision_limit"].as_u64()?).ok()?,
        allowed_egress_hosts: value["allowed_egress_hosts"]
            .as_array()?
            .iter()
            .map(|host| host.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()?,
        confine_agents: value["confine_agents"].as_bool()?,
        min_grade_for_security_review: grade_from(
            value["min_grade_for_security_review"].as_str()?,
        )?,
        require_person_when_check_definitions_change:
            value["require_person_when_check_definitions_change"].as_bool()?,
    })
}

/// Loads the personal policy from `state`, using the default only when the file is absent.
///
/// # Errors
///
/// Returns an error if the file cannot be read, is malformed, or violates policy validation.
pub(crate) fn load(state: &Path) -> io::Result<Policy> {
    let text = match std::fs::read_to_string(state.join(POLICY_FILE)) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(Policy::personal_default());
        }
        Err(error) => return Err(error),
    };
    let value: Value = serde_json::from_str(&text)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let policy = policy_from_json(&value)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "policy.json is malformed"))?;
    policy
        .validate()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(policy)
}

/// Validates every rule before atomically replacing the personal policy.
fn save(state: &Path, policy: &Policy) -> io::Result<()> {
    policy
        .validate()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    kaisa_sys::ensure_private_directory(state)?;
    let value = json!({
        "revision_limit": policy.revision_limit,
        "allow_provisional_spec": policy.allow_provisional_spec,
        "require_person_when_tests_weaken": policy.require_person_when_tests_weaken,
        "allowed_egress_hosts": policy.allowed_egress_hosts,
        "confine_agents": policy.confine_agents,
        "min_grade_for_security_review": format!("{:?}", policy.min_grade_for_security_review),
        "require_person_when_check_definitions_change": policy.require_person_when_check_definitions_change,
    });
    let sequence = NEXT_TEMPORARY_FILE.fetch_add(1, Ordering::Relaxed);
    let temporary = state.join(format!(".policy-{}-{sequence}.tmp", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| {
        file.write_all(value.to_string().as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, state.join(POLICY_FILE))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// Applies one CLI value without saving a partially valid policy.
fn change(policy: &mut Policy, key: &str, value: &str) -> Result<(), String> {
    match key {
        "revision_limit" => {
            policy.revision_limit = value
                .parse::<u32>()
                .ok()
                .filter(|limit| *limit <= MAX_REVISION_LIMIT)
                .ok_or(format!(
                    "revision_limit must be between 0 and {MAX_REVISION_LIMIT}"
                ))?;
        }
        "allow_provisional_spec" => {
            policy.allow_provisional_spec = value.parse().map_err(|_| "use true or false")?;
        }
        "confine_agents" => {
            policy.confine_agents = value.parse().map_err(|_| "use true or false")?;
        }
        "allowed_egress_hosts" => {
            policy.allowed_egress_hosts = if value.is_empty() {
                Vec::new()
            } else {
                value.split(',').map(str::to_owned).collect()
            };
        }
        "require_person_when_tests_weaken" => {
            policy.require_person_when_tests_weaken =
                value.parse().map_err(|_| "use true or false")?;
        }
        "require_person_when_check_definitions_change" => {
            policy.require_person_when_check_definitions_change =
                value.parse().map_err(|_| "use true or false")?;
        }
        "min_grade_for_security_review" => {
            policy.min_grade_for_security_review =
                grade_from(value).ok_or("a grade is T0, T1, T2 or T3")?;
        }
        _ => return Err(format!("unknown policy key {key}")),
    }
    policy.validate().map_err(|error| error.to_string())
}

/// Runs `kaisa policy <args>` using the personal state directory.
pub(crate) fn run(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> io::Result<u8> {
    if !matches!(args, [command] if command == "show")
        && !matches!(args, [command, _, _] if command == "set")
    {
        writeln!(err, "{USAGE}")?;
        return Ok(crate::EXIT_USAGE);
    }
    let state = match kaisa_sys::state_location(|name| std::env::var_os(name)) {
        Ok(location) => location.directory,
        Err(error) => {
            writeln!(err, "kaisa: {error}")?;
            return Ok(crate::EXIT_FAILURE);
        }
    };
    run_in_state(args, &state, out, err)
}

/// Executes a command with an explicit state directory for isolated callers and tests.
fn run_in_state(
    args: &[String],
    state: &Path,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> io::Result<u8> {
    // Damaged or no-longer-admitted policy files are the person's to repair: say what is wrong
    // here, where the file is, and how, rather than leaving the binary to exit silently.
    let loaded = match args {
        [command, ..] if command == "show" || command == "set" => match load(state) {
            Ok(policy) => Some(policy),
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                writeln!(
                    err,
                    "kaisa: {}: {error}\nrepair: edit that file so the named rule is true (or \
                     delete it to return to the defaults), then run the command again",
                    state.join(POLICY_FILE).display()
                )?;
                return Ok(crate::EXIT_FAILURE);
            }
            Err(error) => return Err(error),
        },
        _ => None,
    };
    match args {
        [command] if command == "show" => {
            let policy = loaded.unwrap_or_else(Policy::personal_default);
            out.write_all(policy.canonical_text().as_bytes())?;
        }
        [command, key, value] if command == "set" => {
            let mut policy = loaded.unwrap_or_else(Policy::personal_default);
            if let Err(problem) = change(&mut policy, key, value) {
                writeln!(err, "kaisa policy set: {problem}\n{USAGE}")?;
                return Ok(crate::EXIT_USAGE);
            }
            save(state, &policy)?;
            if key == "confine_agents" && !policy.confine_agents {
                writeln!(
                    err,
                    "warning: agent confinement is off; runs will be reduced"
                )?;
            }
        }
        _ => {
            writeln!(err, "{USAGE}")?;
            return Ok(crate::EXIT_USAGE);
        }
    }
    Ok(crate::EXIT_SUCCESS)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use kaisa_domain::Policy;

    use super::{MAX_REVISION_LIMIT, NEXT_TEMPORARY_FILE, POLICY_FILE, load, run_in_state};

    const UPDATED_REVISION_LIMIT: u32 = 5;

    /// A private state directory that is removed after each policy scenario.
    struct PersonalState(PathBuf);

    impl PersonalState {
        fn new() -> Self {
            let sequence = NEXT_TEMPORARY_FILE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "kaisa-policy-test-{}-{sequence}",
                std::process::id()
            ));
            std::fs::create_dir(&path).expect("the policy scenario gets its own state directory");
            Self(path)
        }

        fn command(&self, words: &[&str]) -> (u8, String, String) {
            let args: Vec<String> = words.iter().map(|word| (*word).to_owned()).collect();
            let mut out = Vec::new();
            let mut err = Vec::new();
            let status = run_in_state(&args, &self.0, &mut out, &mut err)
                .expect("the policy scenario can read and write its state");
            (
                status,
                String::from_utf8(out).expect("policy output is UTF-8"),
                String::from_utf8(err).expect("policy diagnostics are UTF-8"),
            )
        }
    }

    impl Drop for PersonalState {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_policy_change_is_kept_for_the_next_run() {
        let state = PersonalState::new();
        let limit = UPDATED_REVISION_LIMIT.to_string();
        let (status, _, _) = state.command(&["set", "revision_limit", &limit]);
        assert_eq!(
            status,
            crate::EXIT_SUCCESS,
            "a valid revision budget is saved"
        );
        let mut expected = Policy::personal_default();
        expected.revision_limit = UPDATED_REVISION_LIMIT;
        assert_eq!(
            load(&state.0).expect("the next run loads the saved policy"),
            expected,
            "a future run receives the changed revision budget and all other rules"
        );
        let (status, text, _) = state.command(&["show"]);
        assert_eq!(
            status,
            crate::EXIT_SUCCESS,
            "showing a saved policy succeeds"
        );
        assert_eq!(
            text,
            expected.canonical_text(),
            "show prints the domain's canonical policy"
        );
        for (key, value) in [
            ("allowed_egress_hosts", "example.com,api.openai.com"),
            ("require_person_when_check_definitions_change", "true"),
            ("require_person_when_tests_weaken", "true"),
            ("min_grade_for_security_review", "T3"),
        ] {
            let (status, _, _) = state.command(&["set", key, value]);
            assert_eq!(
                status,
                crate::EXIT_SUCCESS,
                "{key} accepts a valid personal choice"
            );
        }
        expected.allowed_egress_hosts = vec!["example.com".into(), "api.openai.com".into()];
        assert_eq!(
            load(&state.0).expect("all personal choices are saved"),
            expected,
            "editing another rule preserves prior personal choices"
        );
        state.command(&["set", "allowed_egress_hosts", ""]);
        assert!(
            load(&state.0)
                .expect("the cleared allow-list is saved")
                .allowed_egress_hosts
                .is_empty(),
            "an empty CLI value clears every egress permission"
        );
    }

    #[test]
    fn an_invalid_policy_is_not_saved() {
        let state = PersonalState::new();
        state.command(&["set", "revision_limit", &UPDATED_REVISION_LIMIT.to_string()]);
        let original = std::fs::read(state.0.join(POLICY_FILE)).expect("a valid policy was saved");
        let excessive_limit = (MAX_REVISION_LIMIT + 1).to_string();
        for (key, value) in [
            ("revision_limit", excessive_limit.as_str()),
            ("revision_limit", "-1"),
            ("confine_agents", "yes"),
            ("allowed_egress_hosts", "api.openai.com,"),
            ("require_person_when_check_definitions_change", "no"),
            ("require_person_when_check_definitions_change", "false"),
            ("require_person_when_tests_weaken", "no"),
            ("require_person_when_tests_weaken", "false"),
            ("min_grade_for_security_review", "T0"),
            ("min_grade_for_security_review", "T1"),
            ("min_grade_for_security_review", "T2"),
            ("min_grade_for_security_review", "T4"),
            ("unknown", "true"),
        ] {
            let (status, _, diagnostic) = state.command(&["set", key, value]);
            assert_eq!(
                status,
                crate::EXIT_USAGE,
                "{key}={value} is not an admitted policy edit"
            );
            assert!(
                diagnostic.contains("usage:"),
                "a rejected edit teaches policy usage"
            );
            assert_eq!(
                std::fs::read(state.0.join(POLICY_FILE)).expect("the prior policy survives"),
                original,
                "invalid edits never replace the admitted policy"
            );
        }
        for words in [
            vec![],
            vec!["show", "extra"],
            vec!["set", "revision_limit"],
            vec!["unknown"],
        ] {
            let (status, _, diagnostic) = state.command(&words);
            assert_eq!(
                status,
                crate::EXIT_USAGE,
                "incomplete or unknown commands are usage errors"
            );
            assert!(
                diagnostic.contains("usage:"),
                "bad command shapes teach the accepted syntax"
            );
        }
    }

    #[test]
    fn without_a_policy_file_the_personal_default_applies() {
        let state = PersonalState::new();
        assert_eq!(
            load(&state.0).expect("a missing policy uses the personal default"),
            Policy::personal_default(),
            "new personal runs use the domain default"
        );
        assert!(
            !state.0.join(POLICY_FILE).exists(),
            "reading the default does not persist a policy"
        );
    }

    #[test]
    fn a_malformed_policy_file_is_an_error_not_the_default() {
        let state = PersonalState::new();
        for text in [
            "not JSON",
            "{}",
            r#"{"revision_limit":2,"allowed_egress_hosts":[],"confine_agents":true,"min_grade_for_security_review":"T2","require_person_when_check_definitions_change":true}"#,
        ] {
            std::fs::write(state.0.join(POLICY_FILE), text)
                .expect("the scenario installs a damaged policy");
            let error = load(&state.0).expect_err("a damaged policy must not admit a default run");
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::InvalidData,
                "malformed and invalid policies are reported as damaged data"
            );
        }
    }

    #[test]
    fn turning_off_the_person_gate_is_refused_with_a_message_naming_the_key() {
        let state = PersonalState::new();
        for key in [
            "require_person_when_tests_weaken",
            "require_person_when_check_definitions_change",
        ] {
            let (status, _, diagnostic) = state.command(&["set", key, "false"]);
            assert_eq!(status, crate::EXIT_USAGE, "{key}=false is refused");
            assert!(
                diagnostic.contains(key) && diagnostic.contains("always stops for a person"),
                "the refusal says the gate cannot be lifted: {diagnostic}"
            );
        }
    }

    #[test]
    fn an_older_policy_file_that_switched_the_person_gate_off_is_refused_not_obeyed() {
        let state = PersonalState::new();
        for key in [
            "require_person_when_tests_weaken",
            "require_person_when_check_definitions_change",
        ] {
            let mut policy = serde_json::json!({
                "revision_limit": 2, "allowed_egress_hosts": [], "confine_agents": true,
                "min_grade_for_security_review": "T3",
                "require_person_when_check_definitions_change": true,
                "require_person_when_tests_weaken": true,
            });
            policy[key] = serde_json::json!(false);
            std::fs::write(state.0.join(POLICY_FILE), policy.to_string())
                .expect("the scenario installs an older policy");
            let error = load(&state.0).expect_err("a policy that lifts the gate is not admitted");
            assert!(
                error.to_string().contains(key),
                "the error names {key}: {error}"
            );
        }
    }

    #[test]
    fn the_policy_command_explains_an_older_file_that_switched_the_gate_off() {
        let state = PersonalState::new();
        for key in [
            "require_person_when_tests_weaken",
            "require_person_when_check_definitions_change",
        ] {
            let mut policy = serde_json::json!({
                "revision_limit": 2, "allowed_egress_hosts": [], "confine_agents": true,
                "min_grade_for_security_review": "T3",
                "require_person_when_check_definitions_change": true,
                "require_person_when_tests_weaken": true,
            });
            policy[key] = serde_json::json!(false);
            std::fs::write(state.0.join(POLICY_FILE), policy.to_string())
                .expect("the scenario installs an older policy");
            for words in [vec!["show"], vec!["set", "revision_limit", "3"]] {
                let (status, out, diagnostic) = state.command(&words);
                assert_eq!(
                    status,
                    crate::EXIT_FAILURE,
                    "{words:?} refuses the old file"
                );
                assert!(out.is_empty(), "nothing is printed as a policy: {out:?}");
                assert!(
                    diagnostic.contains(key)
                        && diagnostic.contains(POLICY_FILE)
                        && diagnostic.contains("repair:"),
                    "{words:?} names {key}, the file and the repair: {diagnostic}"
                );
            }
        }
    }

    #[test]
    fn turning_confinement_off_warns_that_runs_are_reduced() {
        let state = PersonalState::new();
        let (status, _, diagnostic) = state.command(&["set", "confine_agents", "false"]);
        assert_eq!(
            status,
            crate::EXIT_SUCCESS,
            "a person may disable agent confinement"
        );
        assert!(
            diagnostic.contains("warning:") && diagnostic.contains("runs will be reduced"),
            "the person learns that unconfined runs have reduced capabilities"
        );
        assert!(
            !load(&state.0)
                .expect("the confinement choice is persisted")
                .confine_agents,
            "future runs receive the person's confinement choice"
        );
    }
}
