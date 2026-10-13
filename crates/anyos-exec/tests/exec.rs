use anyos_exec::{run, RunOptions};

fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/bin/other")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("fixture {name}: {e}"))
}

/// Runs the static hello-world fixture and checks it exits 0.
/// On Linux this is a real subprocess; on Windows it goes through the VEH engine.
#[test]
#[cfg(any(target_os = "linux", target_os = "windows"))]
fn static_hello_runs() {
    let bytes = fixture("linux-x64-static");
    let result = run(
        &bytes,
        &RunOptions {
            argv: &["hello"],
            envp: &[],
        },
    )
    .expect("run failed");
    assert_eq!(result.exit_code, 0, "guest exited with non-zero code");
}

/// Attempting to run an ARM64 binary on an x64 host is an error, not a panic.
#[test]
fn arm64_rejected_on_x64() {
    let bytes = fixture("linux-arm64");
    let err = run(
        &bytes,
        &RunOptions {
            argv: &["arm"],
            envp: &[],
        },
    );
    assert!(
        matches!(err, Err(anyos_exec::RunError::HostMismatch)),
        "expected HostMismatch, got {err:?}"
    );
}
