//! Detection and planning against the ELF / Mach-O fixtures in tests/fixtures/bin/other
//! (built by tests/fixtures/src/build-other.sh) and one PE fixture.

use std::path::PathBuf;

use anyos_format::{detect, plan, Cpu, Detected, GuestMode, GuestOs, Kind, ProgramSummary};
use anyos_pe::report::{HostArch, HostOs, HostPlatform};

fn read(rel: &str) -> Vec<u8> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/bin")
        .join(rel);
    std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn summary(rel: &str) -> ProgramSummary {
    match detect(&read(rel)).unwrap() {
        Detected::Other(s) => s,
        Detected::Pe(_) => panic!("{rel} detected as PE"),
    }
}

fn mac_arm() -> HostPlatform {
    HostPlatform::new(HostOs::MacOs, HostArch::Aarch64)
}

fn linux_x64() -> HostPlatform {
    HostPlatform::new(HostOs::Linux, HostArch::X86_64)
}

#[test]
fn pe_goes_to_the_pe_parser() {
    assert!(matches!(
        detect(&read("x64/hello_console.exe")).unwrap(),
        Detected::Pe(_)
    ));
}

#[test]
fn dynamic_elf() {
    let s = summary("other/linux-x64");
    assert_eq!(s.format, "ELF");
    assert_eq!(s.os, GuestOs::Linux);
    assert!(s.os_note.is_none());
    let sl = &s.slices[0];
    assert_eq!(
        (sl.cpu.clone(), sl.bits, sl.kind),
        (Cpu::X64, 64, Kind::Executable)
    );
    assert!(sl.interpreter.as_deref().unwrap().contains("ld-linux"));
    assert!(sl.libraries.iter().any(|l| l.starts_with("libc.so")));
    assert!(!sl.statically_linked);

    assert_eq!(plan(&s, linux_x64()).mode, GuestMode::HostNative);
    let p = plan(&s, mac_arm());
    assert_eq!(p.mode, GuestMode::Translated);
    assert!(p.personality.unwrap().contains("Linux"));

    let arm = summary("other/linux-arm64");
    assert_eq!(arm.slices[0].cpu, Cpu::Arm64);
    assert_eq!(plan(&arm, mac_arm()).mode, GuestMode::NativeCpu);
    assert_eq!(plan(&arm, linux_x64()).mode, GuestMode::Translated);
}

#[test]
fn static_elf_is_assumed_linux() {
    let s = summary("other/linux-x64-static");
    let sl = &s.slices[0];
    assert!(sl.statically_linked);
    assert!(sl.interpreter.is_none() && sl.libraries.is_empty());
    assert_eq!(s.os, GuestOs::Linux);
    assert!(s.os_note.is_some());
}

#[test]
fn mach_o_thin_and_universal() {
    let arm = summary("other/macos-arm64");
    assert_eq!(arm.format, "Mach-O");
    assert_eq!(arm.os, GuestOs::MacOs);
    assert_eq!(arm.slices.len(), 1);
    assert_eq!(arm.slices[0].cpu, Cpu::Arm64);
    assert!(arm.slices[0].min_os_version.is_some());
    assert_eq!(arm.slices[0].libraries, ["/usr/lib/libSystem.B.dylib"]);
    assert_eq!(plan(&arm, mac_arm()).mode, GuestMode::HostNative);
    assert_eq!(plan(&arm, linux_x64()).mode, GuestMode::Translated);

    // x86-64 Mac program on an Apple Silicon Mac: macOS's own Rosetta handles it.
    let x64 = summary("other/macos-x64");
    let p = plan(&x64, mac_arm());
    assert_eq!(p.mode, GuestMode::HostNative);
    assert!(p.note.contains("Rosetta"));
    assert_eq!(plan(&x64, linux_x64()).mode, GuestMode::NativeCpu);

    let uni = summary("other/macos-universal");
    let cpus: Vec<_> = uni.slices.iter().map(|s| s.cpu.clone()).collect();
    assert!(cpus.contains(&Cpu::X64) && cpus.contains(&Cpu::Arm64));
    // The planner picks the slice matching the host CPU.
    let p = plan(&uni, mac_arm());
    assert_eq!(uni.slices[p.slice].cpu, Cpu::Arm64);
    let p = plan(&uni, linux_x64());
    assert_eq!(
        (p.mode, uni.slices[p.slice].cpu.clone()),
        (GuestMode::NativeCpu, Cpu::X64)
    );
}
