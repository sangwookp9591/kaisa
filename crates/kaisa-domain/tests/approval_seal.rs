//! Check the exact diagnostics for the external forgery example on Unix, where
//! the injectable boundary exists. Rustdoc's error-code tags alone are not
//! enforced by stable rustdoc.
#![cfg(unix)]

use std::fs;
use std::process::Command;

#[test]
fn external_approval_forgery_is_rejected_by_privacy() {
    let source = include_str!("../src/spec.rs");
    let snippet = source
        .split_once("/// ```compile_fail,E0603\n")
        .expect("approval seal regression fixture and compiler output must be valid")
        .1
        .split_once("    /// ```")
        .expect("approval seal regression fixture and compiler output must be valid")
        .0
        .lines()
        .map(|line| {
            line.strip_prefix("    /// ")
                .expect("approval seal regression fixture and compiler output must be valid")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let scratch = std::env::temp_dir().join(format!("kaisa-approval-seal-{}", std::process::id()));
    fs::create_dir_all(scratch.join("src"))
        .expect("approval seal regression fixture and compiler output must be valid");
    let domain = env!("CARGO_MANIFEST_DIR");
    fs::write(
        scratch.join("Cargo.toml"),
        format!(
            "[package]\nname = \"external-approval-forgery\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[dependencies]\nkaisa-domain = {{ path = {domain:?} }}\n[workspace]\n"
        ),
    )
    .expect("approval seal regression fixture and compiler output must be valid");
    fs::write(
        scratch.join("src/main.rs"),
        format!("fn main() {{\n{snippet}\n}}\n"),
    )
    .expect("approval seal regression fixture and compiler output must be valid");
    let output = Command::new(env!("CARGO"))
        .args(["check", "--offline", "--message-format=json"])
        .arg("--manifest-path")
        .arg(scratch.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(scratch.join("target"))
        .output()
        .expect("approval seal regression fixture and compiler output must be valid");
    fs::remove_dir_all(&scratch)
        .expect("approval seal regression fixture and compiler output must be valid");
    let diagnostics: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .expect("approval seal regression fixture and compiler output must be valid")
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|value| {
            value["reason"] == "compiler-message" && value["message"]["level"] == "error"
        })
        .collect();
    assert!(
        !output.status.success(),
        "external forgery unexpectedly compiled"
    );
    assert_eq!(diagnostics.len(), 2, "{diagnostics:#?}");
    for (diagnostic, name) in diagnostics.iter().zip(["ApprovalTerminal", "confirm"]) {
        assert_eq!(
            diagnostic["message"]["code"]["code"], "E0603",
            "{diagnostic:#?}"
        );
        let message = diagnostic["message"]["message"]
            .as_str()
            .expect("approval seal regression fixture and compiler output must be valid");
        assert!(
            message.contains(name) && message.contains("private"),
            "{message}"
        );
    }
}
