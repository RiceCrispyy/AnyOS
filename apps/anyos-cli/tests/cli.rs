//! End-to-end tests of the `anyos` binary.

use std::path::PathBuf;
use std::process::{Command, Output};

fn fixture(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(rel)
}

fn anyos(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_anyos"))
        .args(args)
        .output()
        .expect("run anyos")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn inspect_prints_a_summary_and_report() {
    let f = fixture("bin/x64/hello_console.exe");
    let o = anyos(&["inspect", f.to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = stdout(&o);
    for want in [
        "PE32+ executable",
        "x86-64",
        "Windows console",
        "kernel32.dll",
        "[x] PE loading",
        "Readiness:",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
}

#[test]
fn inspect_json_is_versioned() {
    let f = fixture("bin/arm64/uses_dll.exe");
    let o = anyos(&["inspect", "--json", f.to_str().unwrap()]);
    assert!(o.status.success());
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["report"]["report_version"], 2);
    assert_eq!(v["image"]["machine"], "Arm64");
    assert_eq!(v["report"]["readiness"], "not_yet_supported");
    // exports_dll.dll sits in the same directory.
    let deps = v["report"]["dependencies"].as_array().unwrap();
    let app = deps.iter().find(|d| d["dll"] == "exports_dll.dll").unwrap();
    assert_eq!(app["location"], "found_beside_image");
}

#[test]
fn inspect_rejects_unknown_files() {
    let o = anyos(&["inspect", fixture("src/build.sh").to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("not a recognised executable format"));
}

#[test]
fn load_binds_application_dll() {
    let f = fixture("bin/x86/uses_dll.exe");
    let o = anyos(&["load", "--rebase", f.to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = stdout(&o);
    assert!(out.contains("Module exports_dll.dll"), "{out}");
    assert!(
        out.contains("2 to guest modules, 1 to the API call gate"),
        "{out}"
    );
    assert!(out.contains("rebased from 0x400000"), "{out}");
}

#[test]
fn load_json_lists_modules_and_gate() {
    let f = fixture("bin/x64/hello_console.exe");
    let o = anyos(&["load", "--json", f.to_str().unwrap()]);
    assert!(o.status.success());
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["modules"].as_array().unwrap().len(), 1);
    assert_eq!(v["call_gate"].as_array().unwrap().len(), 3);
    assert!(
        v["modules"][0]["tls"]["callbacks"]
            .as_array()
            .unwrap()
            .len()
            == 1
    );
}

#[test]
fn redirects_live_in_a_per_program_profile() {
    let dir = std::env::temp_dir().join(format!("anyos-cli-redirect-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let game = dir.join("Game.exe");
    std::fs::copy(fixture("bin/x86/hello_console.exe"), &game).unwrap();
    let g = game.to_str().unwrap();

    let o = anyos(&["redirect", "add", g, "oldgame.example", "203.0.113.10"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let profile = std::fs::read_to_string(dir.join("Game.anyos.toml")).unwrap();
    assert!(profile.contains("203.0.113.10"), "{profile}");

    let out = stdout(&anyos(&["redirect", "test", g, "game.oldgame.example"]));
    assert!(
        out.contains("-> 203.0.113.10:80") && out.contains("rule oldgame.example"),
        "{out}"
    );
    let out = stdout(&anyos(&["redirect", "test", g, "example.com:443"]));
    assert!(out.contains("unchanged"), "{out}");

    // inspect shows the program's redirects, in text and JSON.
    let out = stdout(&anyos(&["inspect", g]));
    assert!(out.contains("Network redirects"), "{out}");
    let v: serde_json::Value =
        serde_json::from_slice(&anyos(&["inspect", "--json", g]).stdout).unwrap();
    assert_eq!(v["redirects"]["oldgame.example"], "203.0.113.10");

    // Bad targets are rejected without touching the profile.
    assert!(!anyos(&["redirect", "add", g, "x.com", "bad host!"])
        .status
        .success());

    assert!(anyos(&["redirect", "remove", g, "oldgame.example"])
        .status
        .success());
    let out = stdout(&anyos(&["redirect", "list", g]));
    assert!(out.contains("no redirects"), "{out}");
    assert!(!anyos(&["redirect", "remove", g, "oldgame.example"])
        .status
        .success());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn inspect_handles_linux_and_mac_programs() {
    let f = fixture("bin/other/linux-x64");
    let o = anyos(&["inspect", "--host", "macos-arm64", f.to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = stdout(&o);
    for want in [
        "ELF",
        "Built for:       Linux",
        "libc.so.6",
        "Linux system calls",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }

    let f = fixture("bin/other/macos-universal");
    let v: serde_json::Value = serde_json::from_slice(
        &anyos(&[
            "inspect",
            "--json",
            "--host",
            "linux-x64",
            f.to_str().unwrap(),
        ])
        .stdout,
    )
    .unwrap();
    assert_eq!(v["program"]["format"], "Mach-O");
    assert_eq!(v["program"]["slices"].as_array().unwrap().len(), 2);
    assert_eq!(v["plan"]["mode"], "native_cpu");
}
