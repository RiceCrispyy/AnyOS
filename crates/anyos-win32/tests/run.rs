//! Runs the PE fixtures end to end on the interpreter.

use std::path::PathBuf;

use anyos_win32::{run_executable, RunError, RunOptions, RunOutcome, Stdio};

fn fixture(arch: &str, name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/bin")
        .join(arch)
        .join(name)
}

fn run(
    arch: &str,
    name: &str,
    opts: &RunOptions,
) -> (Result<RunOutcome, RunError>, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let r = run_executable(
        &fixture(arch, name),
        opts,
        Stdio {
            stdout: &mut out,
            stderr: &mut err,
        },
    );
    (
        r,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

#[test]
fn hello_console_x64_prints_and_exits() {
    let (r, out, _) = run("x64", "hello_console.exe", &RunOptions::default());
    let r = r.expect("run");
    assert_eq!(out, "hello from anyos fixture\nsecond line\n");
    assert_eq!(r.exit_code, 0);
    // GetStdHandle, WriteFile twice, ExitProcess.
    assert_eq!(r.api_calls, 4);
    assert!(r.instructions > 10);
}

#[test]
fn hello_console_x86_prints_and_exits() {
    let (r, out, _) = run("x86", "hello_console.exe", &RunOptions::default());
    assert_eq!(r.expect("run").exit_code, 0);
    assert_eq!(out, "hello from anyos fixture\nsecond line\n");
}

#[test]
fn dll_calls_and_ordinal_imports_work() {
    // anyos_add(2, 3) + anyos_hidden() == 47, passed to ExitProcess.
    for arch in ["x64", "x86"] {
        let (r, _, _) = run(arch, "uses_dll.exe", &RunOptions::default());
        assert_eq!(r.expect(arch).exit_code, 47, "{arch}");
    }
}

#[test]
fn trace_names_each_api_call() {
    let opts = RunOptions {
        trace: true,
        ..RunOptions::default()
    };
    let (r, _, err) = run("x64", "hello_console.exe", &opts);
    r.expect("run");
    assert!(
        err.contains("kernel32.dll!GetStdHandle(0xfffffff5) -> 0x14"),
        "{err}"
    );
    assert!(
        err.contains("kernel32.dll!ExitProcess(0x0) -> exit 0"),
        "{err}"
    );
}

#[test]
fn arm64_needs_another_backend() {
    let (r, _, _) = run("arm64", "hello_console.exe", &RunOptions::default());
    assert!(
        matches!(r, Err(RunError::UnsupportedMachine { .. })),
        "{r:?}"
    );
}

#[test]
fn dlls_are_not_programs() {
    let (r, _, _) = run("x64", "exports_dll.dll", &RunOptions::default());
    assert!(matches!(r, Err(RunError::NotAnExecutable(_))), "{r:?}");
}

#[test]
fn instruction_limit_stops_the_guest() {
    let opts = RunOptions {
        instruction_limit: Some(3),
        ..RunOptions::default()
    };
    let (r, _, _) = run("x64", "hello_console.exe", &opts);
    assert!(matches!(r, Err(RunError::InstructionLimit(3))), "{r:?}");
}

/// The self-test program's output must match the same code run natively on a
/// real x86 CPU (see tests/fixtures/src/build-selftest.sh).
#[test]
fn selftest_matches_real_cpu() {
    for arch in ["x64", "x86"] {
        let expected = std::fs::read_to_string(fixture(arch, "selftest.expected")).unwrap();
        let (r, out, _) = run(arch, "selftest.exe", &RunOptions::default());
        assert_eq!(r.expect(arch).exit_code, 0, "{arch}");
        assert_eq!(out, expected, "{arch}");
    }
}

/// LoadLibrary, GetProcAddress and friends, checked from inside the guest:
/// each passing check sets one bit of the exit code (see dynload.c).
#[test]
fn run_time_linking_works() {
    for arch in ["x64", "x86"] {
        let (r, _, err) = run(arch, "dynload.exe", &RunOptions::default());
        assert_eq!(r.expect(arch).exit_code, 0x7FF, "{arch}: {err}");
    }
}
