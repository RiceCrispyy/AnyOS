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

/// Heaps, virtual memory, TLS, threads and synchronisation, files, wsprintf,
/// code pages, the registry, time, the environment and system information,
/// checked from inside the guest (see apitest.c). Runs in a scratch
/// environment so the registry it writes is saved and can be checked.
#[test]
fn kernel32_apis_behave_like_windows() {
    for arch in ["x64", "x86"] {
        let env = std::env::temp_dir().join(format!("anyos-apitest-{arch}-{}", std::process::id()));
        let drive_c = env.join("drive_c");
        std::fs::create_dir_all(&drive_c).unwrap();
        let registry_file = env.join("registry.toml");
        let opts = RunOptions {
            drive_c: Some(drive_c.clone()),
            registry_file: Some(registry_file.clone()),
            user: Some("tester".into()),
            ..RunOptions::default()
        };
        let (r, out, err) = run(arch, "apitest.exe", &opts);
        let outcome = r.unwrap_or_else(|e| panic!("{arch}: {e}\n{out}{err}"));
        assert_eq!(
            (outcome.exit_code, out.as_str()),
            (0, "ok\n"),
            "{arch}: {err}"
        );
        // The temporary files went to the user's Temp folder inside drive_c,
        // and the test key was deleted again but the registry was written.
        assert!(
            drive_c.join("Users/tester/AppData/Local/Temp").is_dir(),
            "{arch}"
        );
        let saved = std::fs::read_to_string(&registry_file).unwrap();
        assert!(saved.contains("Windows NT"), "{arch}: {saved}");
        assert!(!saved.contains("AnyOS Test"), "{arch}: {saved}");
        std::fs::remove_dir_all(&env).unwrap();
    }
}

/// x64 structured exception handling: a real `__try`/`__except`/`__finally`
/// program, whose exceptions travel through the function table in `.pdata`,
/// the language handler and RtlUnwindEx (see seh64.c).
#[test]
fn x64_structured_exceptions_unwind() {
    let (r, out, err) = run("x64", "seh64.exe", &RunOptions::default());
    let outcome = r.unwrap_or_else(|e| panic!("{e}\n{out}{err}"));
    assert_eq!((outcome.exit_code, out.as_str()), (0, "ok\n"), "{err}");
}

/// The same fixture with no environment at all. A program that is not installed
/// in one still has to be able to write its files: `%APPDATA%`, `%LOCALAPPDATA%`
/// and `%TEMP%` are folders that exist, as they do on Windows.
#[test]
fn apitest_runs_outside_an_environment() {
    for arch in ["x64", "x86"] {
        let (r, out, err) = run(arch, "apitest.exe", &RunOptions::default());
        let outcome = r.unwrap_or_else(|e| panic!("{arch}: {e}\n{out}{err}"));
        assert_eq!(
            (outcome.exit_code, out.as_str()),
            (0, "ok\n"),
            "{arch}: {err}"
        );
    }
}
