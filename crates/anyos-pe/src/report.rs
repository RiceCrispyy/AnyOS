//! Compatibility report: what AnyOS would need to run an image, and how far it is.
//!
//! The report is versioned ([`REPORT_VERSION`]) because later milestones will use
//! it to pick an execution backend and API providers before launch. Capability
//! statuses describe the runtime as it exists today, so they are deliberately
//! conservative: nothing claims to run until an execution backend exists.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::catalog::{classify, ApiFamily};
use crate::{Machine, PeFormat, PeImage, Subsystem};

/// Bump when fields are removed or change meaning.
pub const REPORT_VERSION: u32 = 2;

/// CPU architecture of the machine AnyOS runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum HostArch {
    X86_64,
    Aarch64,
    Other,
}

impl HostArch {
    pub fn current() -> Self {
        match std::env::consts::ARCH {
            "x86_64" => HostArch::X86_64,
            "aarch64" => HostArch::Aarch64,
            _ => HostArch::Other,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            HostArch::X86_64 => "x86-64",
            HostArch::Aarch64 => "ARM64",
            HostArch::Other => "other",
        }
    }
}

/// Operating system AnyOS runs on. ChromeOS runs AnyOS inside its Linux
/// environment, so it reports as Linux.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum HostOs {
    Linux,
    MacOs,
    Other,
}

impl HostOs {
    pub fn current() -> Self {
        match std::env::consts::OS {
            "linux" => HostOs::Linux,
            "macos" => HostOs::MacOs,
            _ => HostOs::Other,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            HostOs::Linux => "Linux",
            HostOs::MacOs => "macOS",
            HostOs::Other => "other OS",
        }
    }
}

/// The host as far as execution planning cares: OS, CPU and page size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct HostPlatform {
    pub os: HostOs,
    pub arch: HostArch,
    /// Smallest unit the host can protect. Windows images assume 4 KiB.
    pub page_size: u64,
}

impl HostPlatform {
    pub fn new(os: HostOs, arch: HostArch) -> Self {
        // Apple Silicon macOS uses 16 KiB pages; the other supported hosts use 4 KiB.
        let page_size = if os == HostOs::MacOs && arch == HostArch::Aarch64 {
            0x4000
        } else {
            0x1000
        };
        HostPlatform {
            os,
            arch,
            page_size,
        }
    }

    pub fn current() -> Self {
        HostPlatform::new(HostOs::current(), HostArch::current())
    }

    pub fn is_apple_silicon(self) -> bool {
        self.os == HostOs::MacOs && self.arch == HostArch::Aarch64
    }

    pub fn display_name(self) -> String {
        if self.is_apple_silicon() {
            "macOS on Apple Silicon".into()
        } else {
            format!("{} {}", self.os.display_name(), self.arch.display_name())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    /// Guest instructions can run directly on the host CPU.
    Native,
    /// Guest instructions run natively in 32-bit compatibility mode (x86 on x86-64).
    NativeCompat32,
    /// Guest instructions need dynamic binary translation or emulation.
    Translated,
    /// No execution path is planned for this combination.
    Unsupported,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecutionPlan {
    pub host: HostPlatform,
    pub guest: Machine,
    pub mode: ExecutionMode,
    pub note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Works today.
    Implemented,
    /// Designed and on the roadmap, not implemented yet.
    Planned,
    /// Out of scope.
    Unsupported,
}

#[derive(Debug, Clone, Serialize)]
pub struct Capability {
    pub name: String,
    pub status: Status,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyLocation {
    /// A Windows system DLL that AnyOS itself has to provide.
    ProvidedByRuntime,
    /// An application DLL found next to the executable.
    FoundBesideImage,
    /// An application DLL that could not be found.
    Missing,
}

#[derive(Debug, Clone, Serialize)]
pub struct Dependency {
    pub dll: String,
    pub family: ApiFamily,
    pub delay_load: bool,
    pub symbols: usize,
    pub location: DependencyLocation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Readiness {
    /// Expected to run on this host.
    Ready,
    /// A supported kind of program, but required runtime pieces are not built yet.
    NotYetSupported,
    /// Outside the project's scope (drivers, EFI, unknown CPUs).
    Unsupported,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompatibilityReport {
    pub report_version: u32,
    pub kind: &'static str,
    pub format: PeFormat,
    pub machine: Machine,
    pub subsystem: Subsystem,
    pub managed: bool,
    pub execution: ExecutionPlan,
    pub dependencies: Vec<Dependency>,
    /// Imported symbol counts per API family (system DLLs only).
    pub api_families: BTreeMap<ApiFamily, usize>,
    pub capabilities: Vec<Capability>,
    /// Host pages (when larger than 4 KiB) that would hold guest pages with
    /// different protections; native execution must handle each one specially.
    pub page_conflicts: Option<PageConflicts>,
    pub readiness: Readiness,
    pub summary: String,
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PageConflicts {
    pub host_page_size: u64,
    pub host_pages: u64,
    pub mixed_pages: u64,
    /// A few examples such as `.text r-x / .rdata r--`.
    pub examples: Vec<String>,
}

impl CompatibilityReport {
    pub fn missing_dependencies(&self) -> impl Iterator<Item = &Dependency> {
        self.dependencies
            .iter()
            .filter(|d| d.location == DependencyLocation::Missing)
    }
}

/// Build a report. `app_dll_exists` answers whether an application DLL with the
/// given name is available beside the image (the CLI checks the file's directory).
pub fn build(
    image: &PeImage,
    host: HostPlatform,
    app_dll_exists: &dyn Fn(&str) -> bool,
) -> CompatibilityReport {
    let mut issues = image.warnings.clone();
    let execution = plan_execution(image, host);

    let mut dependencies = Vec::new();
    let mut api_families: BTreeMap<ApiFamily, usize> = BTreeMap::new();
    for m in &image.imports {
        let family = classify(&m.dll);
        let location = if family.is_system() {
            *api_families.entry(family).or_default() += m.symbols.len();
            DependencyLocation::ProvidedByRuntime
        } else if app_dll_exists(&m.dll) {
            DependencyLocation::FoundBesideImage
        } else {
            DependencyLocation::Missing
        };
        dependencies.push(Dependency {
            dll: m.dll.clone(),
            family,
            delay_load: m.delay_load,
            symbols: m.symbols.len(),
            location,
        });
    }

    let mut caps = Vec::new();
    let mut cap = |name: &str, status: Status, detail: String| {
        caps.push(Capability {
            name: name.into(),
            status,
            detail,
        })
    };
    cap(
        "PE inspection",
        Status::Implemented,
        "headers, sections, imports, exports, relocations, TLS, CLR".into(),
    );
    let loadable = image.broken_directories.is_empty()
        && !image
            .relocations
            .iter()
            .any(|r| matches!(r.kind, crate::RelocationKind::Unsupported(_)));
    cap(
        "PE loading",
        if loadable {
            Status::Implemented
        } else {
            Status::Unsupported
        },
        "map sections, apply relocations, bind imports to call-gate stubs (emulated guest memory)"
            .into(),
    );
    let backend = match execution.mode {
        ExecutionMode::Native => "native execution backend with a controlled API call gate",
        ExecutionMode::NativeCompat32 => {
            "native 32-bit execution backend with a controlled API call gate"
        }
        ExecutionMode::Translated if host.is_apple_silicon() && image.machine == Machine::X64 => {
            "Rosetta 2 (x86-64 build of AnyOS) first, own translator later"
        }
        ExecutionMode::Translated => {
            "binary translation backend (QEMU TCG / FEX-Emu under evaluation)"
        }
        ExecutionMode::Unsupported => "no execution path for this CPU",
    };
    // IL-only assemblies are executed by the managed runtime, not a CPU backend.
    let managed_only = image
        .clr
        .as_ref()
        .is_some_and(|c| c.il_only && !c.requires_32bit);
    if !managed_only {
        cap(
            "Execution backend",
            if execution.mode == ExecutionMode::Unsupported {
                Status::Unsupported
            } else {
                Status::Planned
            },
            backend.into(),
        );
    }
    if image.format == PeFormat::Pe32 && !managed_only {
        cap(
            "32-bit guest support",
            Status::Planned,
            "PE32 (x86) processes need a 32-bit address space and thunks".into(),
        );
    }
    let needs = |f: ApiFamily| api_families.contains_key(&f);
    if needs(ApiFamily::Kernel) || needs(ApiFamily::Ntdll) || needs(ApiFamily::Crt) {
        cap(
            "Windows API: core (kernel32/ntdll/CRT)",
            Status::Planned,
            "process startup, memory, files, threads, errors".into(),
        );
    }
    if image.subsystem == Subsystem::WindowsGui || needs(ApiFamily::User) || needs(ApiFamily::Gdi) {
        cap(
            "Win32 GUI (user32/gdi32)",
            Status::Planned,
            "windows, message loop, input, basic drawing".into(),
        );
    }
    for (family, what) in [
        (ApiFamily::Advapi, "registry and security"),
        (ApiFamily::Shell, "shell and common controls"),
        (ApiFamily::Com, "COM activation, apartments, marshaling"),
        (
            ApiFamily::Networking,
            "sockets and HTTP mapped to host networking",
        ),
        (ApiFamily::Graphics3d, "Direct3D/OpenGL/Vulkan translation"),
        (ApiFamily::Audio, "audio output mapped to host audio"),
        (ApiFamily::Input, "game controllers"),
        (ApiFamily::Crypto, "CNG/CryptoAPI"),
    ] {
        if needs(family) {
            cap(family.display_name(), Status::Planned, what.into());
        }
    }
    if let Some(clr) = &image.clr {
        let mut detail = format!(
            "managed assembly, metadata {}",
            clr.metadata_version.as_deref().unwrap_or("unknown")
        );
        if !clr.il_only {
            detail.push_str("; mixed-mode (contains native code)");
        }
        cap("Managed runtime (.NET)", Status::Planned, detail);
    }
    let kernel_mode = needs(ApiFamily::KernelMode) || image.subsystem == Subsystem::Native;
    if kernel_mode {
        cap(
            "Kernel-mode code",
            Status::Unsupported,
            "drivers and native-subsystem images are out of scope".into(),
        );
    }

    let page_conflicts =
        (execution.mode == ExecutionMode::Native && !managed_only && host.page_size > 0x1000)
            .then(|| page_conflicts(image, host.page_size));
    if let Some(pc) = page_conflicts.as_ref().filter(|pc| pc.mixed_pages > 0) {
        issues.push(format!(
            "{} of {} host pages ({} KiB) mix section protections (e.g. {}); native execution must merge or emulate them",
            pc.mixed_pages,
            pc.host_pages,
            pc.host_page_size / 1024,
            pc.examples.join("; ")
        ));
    }

    let missing: Vec<&str> = dependencies
        .iter()
        .filter(|d| d.location == DependencyLocation::Missing)
        .map(|d| d.dll.as_str())
        .collect();
    if !missing.is_empty() {
        issues.push(format!(
            "application DLLs not found beside the image: {}",
            missing.join(", ")
        ));
    }
    for d in &image.broken_directories {
        issues.push(format!(
            "the {d} directory is damaged; the loader will refuse this image"
        ));
    }

    let (readiness, summary) = if kernel_mode
        || execution.mode == ExecutionMode::Unsupported
        || matches!(
            image.subsystem,
            Subsystem::EfiApplication
                | Subsystem::EfiBootServiceDriver
                | Subsystem::EfiRuntimeDriver
                | Subsystem::EfiRom
                | Subsystem::Xbox
                | Subsystem::WindowsBootApplication
        ) {
        (
            Readiness::Unsupported,
            "This kind of image is outside AnyOS's scope.".to_string(),
        )
    } else if caps.iter().all(|c| c.status == Status::Implemented) {
        (
            Readiness::Ready,
            "All required capabilities are implemented.".to_string(),
        )
    } else {
        let pending: Vec<&str> = caps
            .iter()
            .filter(|c| c.status == Status::Planned)
            .map(|c| c.name.as_str())
            .collect();
        (
            Readiness::NotYetSupported,
            format!("Not runnable yet; still needed: {}.", pending.join(", ")),
        )
    };

    CompatibilityReport {
        report_version: REPORT_VERSION,
        kind: if image.is_dll { "dll" } else { "executable" },
        format: image.format,
        machine: image.machine,
        subsystem: image.subsystem,
        managed: image.clr.is_some(),
        execution,
        dependencies,
        api_families,
        capabilities: caps,
        page_conflicts,
        readiness,
        summary,
        issues,
    }
}

fn plan_execution(image: &PeImage, host: HostPlatform) -> ExecutionPlan {
    use ExecutionMode::*;
    let plan = |mode, note: &str| ExecutionPlan {
        host,
        guest: image.machine,
        mode,
        note: note.into(),
    };
    // IL-only assemblies are CPU-neutral (unless marked 32-bit required): the
    // managed runtime JIT-compiles them for the host.
    if let Some(clr) = &image.clr {
        if clr.il_only && !clr.requires_32bit {
            return plan(
                Native,
                "IL-only assembly: runs on a host-native .NET runtime",
            );
        }
    }
    let mac = host.os == HostOs::MacOs;
    match (image.machine, host.arch) {
        (_, HostArch::Other) => plan(Unsupported, "host CPU architecture is not supported"),
        (Machine::ArmNt, _) => plan(Unsupported, "32-bit ARM (Windows RT) is not planned"),
        (Machine::Unknown(_), _) => plan(Unsupported, "unknown CPU architecture"),
        (Machine::X64, HostArch::X86_64) => plan(Native, "same instruction set as the host: native execution"),
        (Machine::Arm64, HostArch::Aarch64) if mac => plan(
            Native,
            "same instruction set, but macOS reserves register x18 (the Windows TEB pointer) and uses 16 KiB pages",
        ),
        (Machine::Arm64, HostArch::Aarch64) => plan(Native, "same instruction set as the host: native execution"),
        (Machine::X64, HostArch::Aarch64) if mac => plan(
            Translated,
            "x86-64 code on Apple Silicon: Rosetta 2 can translate it if AnyOS itself runs as an x86-64 process",
        ),
        (Machine::X64, HostArch::Aarch64) => plan(Translated, "x86-64 code on an ARM64 host: needs binary translation"),
        (Machine::X86, _) if mac => plan(
            Translated,
            "32-bit x86 code: macOS has no 32-bit process support, so it needs 32-bit-on-64 thunking or translation",
        ),
        (Machine::X86, HostArch::X86_64) => plan(
            NativeCompat32,
            "32-bit x86 code on an x86-64 host: native in compatibility mode",
        ),
        (Machine::X86, HostArch::Aarch64) => plan(Translated, "x86 code on an ARM64 host: needs binary translation"),
        (Machine::Arm64, HostArch::X86_64) => plan(
            Translated,
            "ARM64 code on an x86-64 host: needs binary translation/emulation",
        ),
        (Machine::Arm64Ec, HostArch::Aarch64) => plan(
            Native,
            "ARM64EC: native ARM64 code with x64-compatible ABI (x64 parts need translation)",
        ),
        (Machine::Arm64Ec, HostArch::X86_64) => plan(Translated, "ARM64EC on an x86-64 host: needs emulation"),
    }
}

/// Count host pages that would contain guest (4 KiB) pages with different
/// protections, using the same rules as the loader: headers read-only, each
/// section per its characteristics, writable implies readable.
fn page_conflicts(image: &PeImage, host_page: u64) -> PageConflicts {
    const GUEST_PAGE: u64 = 0x1000;
    let size = image.size_of_image as u64;
    let guest_pages = size.div_ceil(GUEST_PAGE) as usize;
    let mut prot: Vec<(String, String)> = vec![("headers".into(), "r--".into()); guest_pages];
    if (image.section_alignment as u64) >= GUEST_PAGE {
        for s in &image.sections {
            let a = s.access;
            let flags = crate::SectionAccess {
                read: a.read || a.write,
                ..a
            }
            .flags();
            let start = s.virtual_address as u64 / GUEST_PAGE;
            let end = (s.virtual_address as u64 + s.mapped_size() as u64).div_ceil(GUEST_PAGE);
            for p in start..end.min(guest_pages as u64) {
                prot[p as usize] = (s.name.clone(), flags.clone());
            }
        }
    }
    let per = (host_page / GUEST_PAGE) as usize;
    let mut mixed = 0;
    let mut examples = Vec::new();
    let chunks = prot.chunks(per);
    let host_pages = chunks.len() as u64;
    for chunk in chunks {
        let first = &chunk[0];
        if let Some(other) = chunk.iter().find(|p| p.1 != first.1) {
            mixed += 1;
            if examples.len() < 3 {
                examples.push(format!("{} {} / {} {}", first.0, first.1, other.0, other.1));
            }
        }
    }
    PageConflicts {
        host_page_size: host_page,
        host_pages,
        mixed_pages: mixed,
        examples,
    }
}
