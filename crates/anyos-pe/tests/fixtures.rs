//! Inspector tests against the purpose-built fixtures in `tests/fixtures`.

use std::path::PathBuf;

use anyos_pe::catalog::ApiFamily;
use anyos_pe::report::{
    self, DependencyLocation, ExecutionMode, HostArch, HostOs, HostPlatform, Readiness, Status,
};
use anyos_pe::*;

fn fixture(rel: &str) -> Vec<u8> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(rel);
    std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

const ARCHES: [(&str, Machine, PeFormat); 3] = [
    ("x64", Machine::X64, PeFormat::Pe32Plus),
    ("arm64", Machine::Arm64, PeFormat::Pe32Plus),
    ("x86", Machine::X86, PeFormat::Pe32),
];

fn linux(arch: HostArch) -> HostPlatform {
    HostPlatform::new(HostOs::Linux, arch)
}

fn names(m: &ImportedModule) -> Vec<String> {
    m.symbols.iter().map(|s| s.name.to_string()).collect()
}

#[test]
fn console_executables_are_classified() {
    for (arch, machine, format) in ARCHES {
        let img = parse(&fixture(&format!("bin/{arch}/hello_console.exe"))).unwrap();
        assert_eq!(img.machine, machine, "{arch}");
        assert_eq!(img.format, format, "{arch}");
        assert_eq!(img.subsystem, Subsystem::WindowsConsole);
        assert!(!img.is_dll && !img.is_managed());
        assert!(img.dynamic_base());
        assert!(
            img.broken_directories.is_empty() && img.warnings.is_empty(),
            "{:?}",
            img.warnings
        );

        let entry = img.entry_point_rva.expect("entry point");
        let text = img.section_for_rva(entry).expect("entry in a section");
        assert_eq!(text.name, ".text");
        assert!(text.access.execute && !text.access.write);

        assert_eq!(img.imports.len(), 1);
        assert_eq!(img.imports[0].dll, "kernel32.dll");
        let mut got = names(&img.imports[0]);
        got.sort();
        assert_eq!(got, ["ExitProcess", "GetStdHandle", "WriteFile"]);

        // The `lines` pointer table and the TLS directory need absolute fixups.
        let expected = if format == PeFormat::Pe32 {
            RelocationKind::HighLow
        } else {
            RelocationKind::Dir64
        };
        assert!(img.relocations.len() >= 2);
        assert!(
            img.relocations.iter().all(|r| r.kind == expected),
            "{arch}: {:?}",
            img.relocations
        );

        let tls = img.tls.as_ref().expect("TLS directory");
        assert_eq!(tls.callbacks.len(), 1);
        assert_eq!(tls.raw_data_end_va - tls.raw_data_start_va, 8);
        let cb_rva = (tls.callbacks[0] - img.image_base) as u32;
        assert_eq!(img.section_for_rva(cb_rva).unwrap().name, ".text");
    }
}

#[test]
fn gui_executable_has_delay_loaded_user32() {
    for (arch, ..) in ARCHES {
        let img = parse(&fixture(&format!("bin/{arch}/hello_gui.exe"))).unwrap();
        assert_eq!(img.subsystem, Subsystem::WindowsGui, "{arch}");
        let user32 = img
            .imports
            .iter()
            .find(|m| m.dll == "user32.dll")
            .expect("user32 import");
        assert!(user32.delay_load);
        assert_eq!(names(user32), ["MessageBoxA"]);
        let k32 = img
            .imports
            .iter()
            .find(|m| m.dll == "kernel32.dll")
            .unwrap();
        assert!(!k32.delay_load);

        let r = report::build(&img, linux(HostArch::X86_64), &|_| false);
        let gui = r
            .capabilities
            .iter()
            .find(|c| c.name.starts_with("Win32 GUI"))
            .expect("GUI capability");
        assert_eq!(gui.status, Status::Planned);
        assert!(r
            .dependencies
            .iter()
            .any(|d| d.dll == "user32.dll" && d.delay_load && d.family == ApiFamily::User));
    }
}

#[test]
fn dll_exports_by_name_and_ordinal() {
    for (arch, ..) in ARCHES {
        let img = parse(&fixture(&format!("bin/{arch}/exports_dll.dll"))).unwrap();
        assert!(img.is_dll, "{arch}");
        let t = img.exports.as_ref().expect("export table");
        assert_eq!(t.dll_name.as_deref(), Some("exports_dll.dll"));
        assert_eq!(t.ordinal_base, 7);
        let by_name = |n: &str| {
            t.exports
                .iter()
                .find(|e| e.name.as_deref() == Some(n))
                .unwrap()
        };
        for n in ["anyos_add", "anyos_mul", "counter_ptr"] {
            match &by_name(n).target {
                ExportTarget::Rva { rva } => assert!(img.section_for_rva(*rva).is_some()),
                other => panic!("{n}: {other:?}"),
            }
        }
        let hidden = t.exports.iter().find(|e| e.ordinal == 7).unwrap();
        assert_eq!(hidden.name, None, "ordinal 7 is NONAME");
        let code = img.section_for_rva(match hidden.target {
            ExportTarget::Rva { rva } => rva,
            _ => unreachable!(),
        });
        assert_eq!(code.unwrap().name, ".text");
        assert!(img.entry_point_rva.is_some(), "DllMain entry");
    }
}

#[test]
fn imports_by_ordinal() {
    for (arch, ..) in ARCHES {
        let img = parse(&fixture(&format!("bin/{arch}/uses_dll.exe"))).unwrap();
        let m = img
            .imports
            .iter()
            .find(|m| m.dll == "exports_dll.dll")
            .expect("{arch}");
        assert_eq!(
            m.symbols[0].name,
            ImportName::Name {
                name: "anyos_add".into(),
                hint: 0
            }
        );
        assert_eq!(m.symbols[1].name, ImportName::Ordinal { ordinal: 7 });
        let ptr = img.format.pointer_size() as u32;
        assert_eq!(m.symbols[1].iat_rva, m.symbols[0].iat_rva + ptr);
    }
}

#[test]
fn managed_assembly_is_detected() {
    let img = parse(&fixture(
        "third_party/System.Runtime.CompilerServices.Unsafe.dll",
    ))
    .unwrap();
    let clr = img.clr.as_ref().expect("CLR header");
    assert!(clr.il_only && !clr.requires_32bit);
    assert!(clr.strong_name_signed);
    assert!(clr.metadata_version.as_deref().unwrap().starts_with("v4.0"));
    assert_eq!(img.imports.len(), 1);
    assert_eq!(img.imports[0].dll.to_ascii_lowercase(), "mscoree.dll");

    let r = report::build(&img, linux(HostArch::Aarch64), &|_| false);
    assert!(r.managed);
    assert_eq!(
        r.execution.mode,
        ExecutionMode::Native,
        "IL-only code is CPU neutral"
    );
    assert!(r
        .capabilities
        .iter()
        .any(|c| c.name.contains(".NET") && c.status == Status::Planned));
    assert!(!r.capabilities.iter().any(|c| c.name == "Execution backend"));
}

#[test]
fn execution_plan_depends_on_host() {
    let x64 = parse(&fixture("bin/x64/hello_console.exe")).unwrap();
    let arm = parse(&fixture("bin/arm64/hello_console.exe")).unwrap();
    let x86 = parse(&fixture("bin/x86/hello_console.exe")).unwrap();
    let mode = |img: &PeImage, arch| report::build(img, linux(arch), &|_| false).execution.mode;
    assert_eq!(mode(&x64, HostArch::X86_64), ExecutionMode::Native);
    assert_eq!(mode(&x64, HostArch::Aarch64), ExecutionMode::Translated);
    assert_eq!(mode(&arm, HostArch::Aarch64), ExecutionMode::Native);
    assert_eq!(mode(&arm, HostArch::X86_64), ExecutionMode::Translated);
    assert_eq!(mode(&x86, HostArch::X86_64), ExecutionMode::NativeCompat32);
    assert_eq!(mode(&x86, HostArch::Aarch64), ExecutionMode::Translated);
}

#[test]
fn report_finds_application_dlls() {
    let img = parse(&fixture("bin/x64/uses_dll.exe")).unwrap();
    let missing = report::build(&img, linux(HostArch::X86_64), &|_| false);
    let dep = missing
        .dependencies
        .iter()
        .find(|d| d.dll == "exports_dll.dll")
        .unwrap();
    assert_eq!(dep.location, DependencyLocation::Missing);
    assert_eq!(dep.family, ApiFamily::Application);
    assert!(missing.issues.iter().any(|i| i.contains("exports_dll.dll")));

    let found = report::build(&img, linux(HostArch::X86_64), &|d| {
        d.eq_ignore_ascii_case("EXPORTS_DLL.DLL")
    });
    assert_eq!(found.missing_dependencies().count(), 0);
    assert_eq!(found.readiness, Readiness::NotYetSupported);
    assert_eq!(found.report_version, report::REPORT_VERSION);
}

#[test]
fn macos_hosts_get_their_own_plan() {
    let mac_arm = HostPlatform::new(HostOs::MacOs, HostArch::Aarch64);
    let mac_intel = HostPlatform::new(HostOs::MacOs, HostArch::X86_64);
    assert_eq!(mac_arm.page_size, 0x4000);
    assert_eq!(mac_intel.page_size, 0x1000);

    let x64 = parse(&fixture("bin/x64/hello_console.exe")).unwrap();
    let r = report::build(&x64, mac_arm, &|_| false);
    assert_eq!(r.execution.mode, ExecutionMode::Translated);
    assert!(r.execution.note.contains("Rosetta 2"));
    assert!(
        r.page_conflicts.is_none(),
        "translated code does not use host pages directly"
    );

    let x86 = parse(&fixture("bin/x86/hello_console.exe")).unwrap();
    assert_eq!(
        report::build(&x86, mac_intel, &|_| false).execution.mode,
        ExecutionMode::Translated
    );

    // ARM64 Windows code is native on Apple Silicon, but these tiny images put
    // .text (r-x), .rdata (r--) and .data (rw-) inside one 16 KiB host page.
    let arm = parse(&fixture("bin/arm64/hello_console.exe")).unwrap();
    let r = report::build(&arm, mac_arm, &|_| false);
    assert_eq!(r.execution.mode, ExecutionMode::Native);
    let pc = r.page_conflicts.as_ref().unwrap();
    assert_eq!(pc.host_page_size, 0x4000);
    assert_eq!(pc.host_pages, 2);
    assert!(pc.mixed_pages >= 1);
    assert!(r.issues.iter().any(|i| i.contains("16 KiB")));

    // 4 KiB hosts never report conflicts.
    assert!(report::build(&arm, linux(HostArch::Aarch64), &|_| false)
        .page_conflicts
        .is_none());
}
