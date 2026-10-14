//! What the picked file is and what the app can do with it.
//!
//! This is the GUI's view of `anyos-format` (detect and plan), `anyos-pe`
//! (compatibility report) and `anyos-package` (installers). It has no egui code,
//! so it is tested like any other library code.

use std::path::{Path, PathBuf};

use anyos_format::{Detected, GuestMode, GuestOs, ProgramSummary};
use anyos_package::{Package, PackageKind};
use anyos_pe::report::{self, ExecutionMode, HostArch, HostOs, HostPlatform, Status};
use anyos_pe::{Machine, PeImage, Subsystem};

/// What the main button does for this file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// `anyos run <program>`.
    Run,
    /// Built for this computer: start it directly, AnyOS is not needed.
    Launch,
    /// `anyos install <installer>`.
    Install,
    /// Nothing can be done yet, for this reason.
    Blocked(String),
}

/// How well the file fits this computer, for the coloured badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Good,
    Partial,
    Bad,
}

#[derive(Debug, Clone)]
pub struct Analysis {
    /// The program file itself (inside the bundle for a macOS `.app`).
    pub program: PathBuf,
    /// What the user picked: the file, or the `.app` folder.
    pub picked: PathBuf,
    pub name: String,
    pub size: u64,
    /// Label/value rows: format, OS, CPU, kind.
    pub facts: Vec<(&'static str, String)>,
    pub badge: String,
    pub tone: Tone,
    /// One or two sentences on how it runs on this host.
    pub plan: String,
    /// Pieces of AnyOS this program needs that are still planned.
    pub still_needed: Vec<String>,
    /// Warnings worth showing (unusual headers, missing DLLs, ...).
    pub notes: Vec<String>,
    pub action: Action,
}

/// Inspect `picked` for `host`. Never runs any of its code.
pub fn analyse(picked: &Path, host: HostPlatform) -> Result<Analysis, String> {
    // Absolute, because programs are started from their own folder.
    let picked = &std::path::absolute(picked).map_err(|e| format!("{}: {e}", picked.display()))?;
    let program = resolve_bundle(picked)?;
    let bytes = std::fs::read(&program).map_err(|e| format!("{}: {e}", program.display()))?;
    let mut a = Analysis {
        program: program.clone(),
        picked: picked.to_path_buf(),
        name: file_name(picked),
        size: bytes.len() as u64,
        facts: Vec::new(),
        badge: String::new(),
        tone: Tone::Bad,
        plan: String::new(),
        still_needed: Vec::new(),
        notes: Vec::new(),
        action: Action::Blocked(String::new()),
    };
    match anyos_format::detect(&bytes) {
        Ok(Detected::Pe(image)) => {
            let package = anyos_package::identify(&bytes, Some(&image));
            analyse_pe(&mut a, &image, package.as_ref(), host);
        }
        Ok(Detected::Other(summary)) => {
            let package = anyos_package::identify(&bytes, None);
            analyse_other(&mut a, &summary, package.as_ref(), host);
        }
        Err(e) => match anyos_package::identify(&bytes, None) {
            Some(p) => analyse_package(&mut a, &p),
            None => return Err(format!("{} is not a program AnyOS recognises: {e}", a.name)),
        },
    }
    Ok(a)
}

fn analyse_pe(a: &mut Analysis, image: &PeImage, package: Option<&Package>, host: HostPlatform) {
    let kind = if image.is_dll {
        "DLL (library)"
    } else {
        match image.subsystem {
            Subsystem::WindowsConsole => "console program",
            Subsystem::WindowsGui => "windowed program",
            _ => "program",
        }
    };
    a.facts
        .push(("Format", format!("Windows {}", image.format.display_name())));
    a.facts.push(("Built for", "Windows".into()));
    a.facts
        .push(("CPU", image.machine.display_name().to_string()));
    a.facts.push(("Kind", kind.into()));
    if image.is_managed() {
        a.facts.push(("Runtime", ".NET".into()));
    }

    let rep = report::build(image, host, &beside(&a.program));
    a.notes.extend(rep.issues.iter().cloned());
    a.still_needed = rep
        .capabilities
        .iter()
        .filter(|c| c.status == Status::Planned)
        .map(|c| c.name.clone())
        .collect();

    if let Some(p) = package.filter(|_| !image.is_dll) {
        a.facts.push(("Installer", p.kind.display_name().into()));
        if can_install(p.kind) {
            a.badge = "Installer".into();
            a.tone = Tone::Good;
            a.plan = format!(
                "AnyOS unpacks this {} into its own environment; none of the installer's code runs.",
                p.kind.display_name()
            );
            a.action = Action::Install;
            return;
        }
    }

    let cpu = match rep.execution.mode {
        ExecutionMode::Native | ExecutionMode::NativeCompat32 => "Same CPU family as this computer",
        ExecutionMode::Translated => {
            "Different CPU from this computer, so its instructions are translated"
        }
        ExecutionMode::Unsupported => "CPU not supported",
    };
    a.plan = format!(
        "{cpu}. Today AnyOS runs Windows programs on its portable interpreter, \
         with most of kernel32 (console programs, no windows yet); it stops with a clear message at the first API it lacks."
    );

    let blocked = if image.is_dll {
        Some("This is a DLL that other programs load. Pick the .exe that uses it.".to_string())
    } else if image.is_managed() {
        Some(".NET programs cannot run yet.".to_string())
    } else if !matches!(image.machine, Machine::X86 | Machine::X64) {
        Some(format!(
            "{} Windows programs cannot run yet; only x86 and x86-64 can.",
            image.machine.display_name()
        ))
    } else {
        None
    };
    match blocked {
        Some(reason) => {
            a.badge = "Not runnable yet".into();
            a.tone = Tone::Bad;
            a.action = Action::Blocked(reason);
        }
        None => {
            a.badge = "Runs on AnyOS (early)".into();
            a.tone = Tone::Partial;
            if image.subsystem == Subsystem::WindowsGui {
                a.notes.insert(
                    0,
                    "Windowed programs need the Windows UI libraries (user32, gdi32), which AnyOS does \
                     not provide yet, so this will likely stop when it opens its first window."
                        .into(),
                );
            }
            // The report's capability list predates the interpreter, which
            // now covers what it calls planned; the run itself shows the gaps.
            a.still_needed.clear();
            a.action = Action::Run;
        }
    }
}

fn analyse_other(
    a: &mut Analysis,
    p: &ProgramSummary,
    package: Option<&Package>,
    host: HostPlatform,
) {
    let plan = anyos_format::plan(p, host);
    a.facts.push(("Format", p.format.to_string()));
    let mut os = p.os.display_name().to_string();
    if let Some(n) = &p.os_note {
        os.push_str(&format!(" ({n})"));
    }
    a.facts.push(("Built for", os));
    let cpus: Vec<String> = p.slices.iter().map(|s| s.cpu.display_name()).collect();
    a.facts
        .push((if cpus.len() > 1 { "CPUs" } else { "CPU" }, cpus.join(", ")));
    if let Some(s) = p.slices.get(plan.slice) {
        a.facts
            .push(("Kind", format!("{:?}", s.kind).to_lowercase()));
    }
    if let Some(pkg) = package {
        a.facts.push(("Installer", pkg.kind.display_name().into()));
    }
    a.plan = plan.note.clone();
    if let Some(layer) = plan.personality {
        a.still_needed.push(layer.to_string());
    }

    let elf_runner = p.format == "ELF"
        && p.os == GuestOs::Linux
        && p.slices.get(plan.slice).map(|s| s.cpu.clone()) == Some(anyos_format::Cpu::X64)
        && host.arch == HostArch::X86_64
        && cfg!(any(target_os = "linux", target_os = "windows"));
    match plan.mode {
        GuestMode::HostNative => {
            a.badge = "Runs directly".into();
            a.tone = Tone::Good;
            a.action = Action::Launch;
        }
        _ if elf_runner => {
            a.badge = "Runs on AnyOS (early)".into();
            a.tone = Tone::Partial;
            a.plan = "x86-64 Linux program on an x86-64 computer: AnyOS loads it and serves its \
                      Linux system calls."
                .into();
            a.still_needed.clear();
            a.action = Action::Run;
        }
        GuestMode::Unsupported => {
            a.badge = "Not supported".into();
            a.tone = Tone::Bad;
            a.action = Action::Blocked(plan.note);
        }
        _ => {
            a.badge = "Not runnable yet".into();
            a.tone = Tone::Bad;
            a.action = Action::Blocked(format!(
                "AnyOS cannot run {} programs on {} yet.",
                p.os.display_name(),
                host.display_name()
            ));
        }
    }
}

fn analyse_package(a: &mut Analysis, p: &Package) {
    a.facts.push(("Installer", p.kind.display_name().into()));
    a.facts
        .push(("Installs", p.kind.os().display_name().into()));
    if can_install(p.kind) {
        a.badge = "Installer".into();
        a.tone = Tone::Good;
        a.plan = "AnyOS unpacks this into its own environment; none of the installer's code runs."
            .into();
        a.action = Action::Install;
    } else {
        a.badge = "Not supported yet".into();
        a.tone = Tone::Bad;
        a.plan = format!("{} files cannot be installed yet.", p.kind.display_name());
        a.action = Action::Blocked(a.plan.clone());
    }
}

/// Kinds `anyos install` handles today (matches the CLI's list).
fn can_install(kind: PackageKind) -> bool {
    matches!(
        kind,
        PackageKind::Msi
            | PackageKind::Nsis
            | PackageKind::Zip
            | PackageKind::ZipSfx
            | PackageKind::SevenZip
            | PackageKind::SevenZipSfx
            | PackageKind::Deb
            | PackageKind::MacPkg
    )
}

/// A macOS `.app` is a folder; the program is `Contents/MacOS/<CFBundleExecutable>`.
/// Any other path is returned unchanged.
pub fn resolve_bundle(picked: &Path) -> Result<PathBuf, String> {
    if !picked.is_dir() {
        return Ok(picked.to_path_buf());
    }
    let macos = picked.join("Contents").join("MacOS");
    if let Ok(plist) = std::fs::read_to_string(picked.join("Contents").join("Info.plist")) {
        if let Some(exe) = plist_string(&plist, "CFBundleExecutable") {
            let p = macos.join(exe);
            if p.is_file() {
                return Ok(p);
            }
        }
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(&macos)
        .map_err(|_| format!("{} is a folder, not a program", picked.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    files
        .into_iter()
        .next()
        .ok_or_else(|| format!("{} has no program in Contents/MacOS", picked.display()))
}

/// Value of `<key>name</key><string>value</string>` in an XML property list.
fn plist_string<'a>(plist: &'a str, key: &str) -> Option<&'a str> {
    let after = &plist[plist.find(&format!("<key>{key}</key>"))?..];
    let start = after.find("<string>")? + "<string>".len();
    let end = after[start..].find("</string>")?;
    Some(after[start..start + end].trim())
}

fn beside(path: &Path) -> impl Fn(&str) -> bool {
    let dir = path.parent().unwrap_or(Path::new("."));
    let names: Vec<String> = std::fs::read_dir(dir)
        .map(|it| {
            it.flatten()
                .map(|e| e.file_name().to_string_lossy().to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default();
    move |dll: &str| names.contains(&dll.to_ascii_lowercase())
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// Short host description for the window header.
pub fn host_label(host: HostPlatform) -> String {
    match host.os {
        HostOs::Other => format!("{} (untested host)", host.display_name()),
        _ => host.display_name(),
    }
}

/// Split an arguments line the way a shell would for simple cases:
/// whitespace separates, and single or double quotes group.
pub fn split_args(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    for c in line.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                any = true;
            }
            None if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            None => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(rel: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/bin")
            .join(rel)
    }

    fn mac_arm() -> HostPlatform {
        HostPlatform::new(HostOs::MacOs, HostArch::Aarch64)
    }

    #[test]
    fn windows_console_program_runs() {
        let a = analyse(&fixture("x64/hello_console.exe"), mac_arm()).unwrap();
        assert_eq!(a.action, Action::Run);
        assert_eq!(a.tone, Tone::Partial);
        assert!(a.facts.contains(&("Kind", "console program".into())));
        assert!(a.plan.contains("translated"), "{}", a.plan);
    }

    #[test]
    fn windows_gui_program_runs_with_a_warning() {
        let a = analyse(&fixture("x86/hello_gui.exe"), mac_arm()).unwrap();
        assert_eq!(a.action, Action::Run);
        assert!(a.notes[0].contains("user32"));
    }

    #[test]
    fn dll_and_arm64_are_blocked() {
        let dll = analyse(&fixture("x64/exports_dll.dll"), mac_arm()).unwrap();
        assert!(matches!(dll.action, Action::Blocked(r) if r.contains("DLL")));
        let arm = analyse(&fixture("arm64/hello_console.exe"), mac_arm()).unwrap();
        assert!(matches!(arm.action, Action::Blocked(r) if r.contains("ARM64")));
    }

    #[test]
    fn native_mac_program_launches_directly() {
        let a = analyse(&fixture("other/macos-arm64"), mac_arm()).unwrap();
        assert_eq!(a.action, Action::Launch);
        assert_eq!(a.tone, Tone::Good);
    }

    #[test]
    fn linux_program_on_mac_is_blocked_with_reason() {
        let a = analyse(&fixture("other/linux-x64"), mac_arm()).unwrap();
        assert!(matches!(a.action, Action::Blocked(r) if r.contains("Linux")));
        assert!(!a.still_needed.is_empty());
    }

    #[test]
    fn unknown_file_is_an_error() {
        let a = analyse(&fixture("x64/selftest.expected"), mac_arm());
        assert!(a.is_err());
    }

    #[test]
    fn app_bundle_resolves_to_its_executable() {
        let dir = std::env::temp_dir().join(format!("anyos-gui-test-{}", std::process::id()));
        let bundle = dir.join("Demo.app");
        let macos = bundle.join("Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        std::fs::write(
            bundle.join("Contents/Info.plist"),
            "<plist><dict><key>CFBundleExecutable</key>\n<string>Demo</string></dict></plist>",
        )
        .unwrap();
        std::fs::copy(fixture("other/macos-arm64"), macos.join("Demo")).unwrap();
        let a = analyse(&bundle, mac_arm()).unwrap();
        assert_eq!(a.program, macos.join("Demo"));
        assert_eq!(a.name, "Demo.app");
        assert_eq!(a.action, Action::Launch);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn split_args_handles_quotes() {
        assert_eq!(
            split_args(r#"-v "two words" 'x y' ""  end"#),
            vec!["-v", "two words", "x y", "", "end"]
        );
        assert!(split_args("   ").is_empty());
    }
}
