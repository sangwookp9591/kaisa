//! Exercise the documented offline example, including its production gate and reports.
#[cfg(target_os = "macos")]
#[test]
fn documented_offline_spec_demo_delivers_only_the_passing_candidate() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::env::temp_dir().join(format!("kaisa-demo-test-{}", std::process::id()));
    let binary = std::path::Path::new(env!("CARGO_BIN_EXE_kaisa"));
    let result = std::process::Command::new("python3")
        .arg(root.join("examples/spec-demo/run.py"))
        .arg("--binary")
        .arg(binary)
        .arg("--fake")
        .arg(binary.with_file_name("kaisa-fake-agent"))
        .arg("--output")
        .arg(&output)
        .output()
        .expect("Python demo harness");
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let fail = std::fs::read_to_string(output.join("07-report-fail.txt")).expect("FAIL report");
    assert!(fail.contains("Fail: private QA all passed on FINAL tree"));
    assert!(!fail.contains("Result: kaisa/result/"));
    std::fs::remove_dir_all(output).expect("remove demo output");
}
