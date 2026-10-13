//! # anyos-format
//!
//! Format-neutral front door for AnyOS: given any file, say what kind of
//! program it is, which OS and CPU it was built for, and what AnyOS would need
//! to run it on the current host.
//!
//! * Windows PE images go through the full inspector in `anyos-pe`.
//! * ELF (Linux, Android, FreeBSD) and Mach-O (macOS, including universal
//!   binaries) get a summary here. These are the first step toward running any
//!   OS's programs on any OS; their loaders and OS personalities come later.
//!
//! Nothing here executes code.

use anyos_pe::report::{HostArch, HostOs, HostPlatform};
use anyos_pe::PeImage;
use goblin::mach::load_command::CommandVariant;
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum FormatError {
    #[error(transparent)]
    Pe(#[from] anyos_pe::PeError),
    #[error("not a recognised executable format (magic 0x{0:x})")]
    Unknown(u64),
    #[error("malformed {format} file: {message}")]
    Malformed {
        format: &'static str,
        message: String,
    },
}

/// OS a program was built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum GuestOs {
    Windows,
    Linux,
    Android,
    FreeBsd,
    MacOs,
    /// iOS, tvOS, watchOS, visionOS or Mac Catalyst.
    AppleMobile,
    Unknown,
}

impl GuestOs {
    pub fn display_name(self) -> &'static str {
        match self {
            GuestOs::Windows => "Windows",
            GuestOs::Linux => "Linux",
            GuestOs::Android => "Android",
            GuestOs::FreeBsd => "FreeBSD",
            GuestOs::MacOs => "macOS",
            GuestOs::AppleMobile => "iOS-family",
            GuestOs::Unknown => "unknown OS",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Cpu {
    X86,
    X64,
    Arm,
    Arm64,
    RiscV64,
    Other(String),
}

impl Cpu {
    pub fn display_name(&self) -> String {
        match self {
            Cpu::X86 => "x86".into(),
            Cpu::X64 => "x86-64".into(),
            Cpu::Arm => "ARM (32-bit)".into(),
            Cpu::Arm64 => "ARM64".into(),
            Cpu::RiscV64 => "RISC-V 64".into(),
            Cpu::Other(s) => s.clone(),
        }
    }

    fn host_arch(&self) -> Option<HostArch> {
        match self {
            Cpu::X64 => Some(HostArch::X86_64),
            Cpu::Arm64 => Some(HostArch::Aarch64),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Executable,
    SharedLibrary,
    Object,
    Other,
}

/// One architecture's worth of a program (universal Mach-O files have several).
#[derive(Debug, Clone, Serialize)]
pub struct Slice {
    pub cpu: Cpu,
    pub bits: u8,
    pub kind: Kind,
    pub entry: Option<u64>,
    /// Shared libraries / dylibs it links against.
    pub libraries: Vec<String>,
    /// ELF program interpreter (dynamic loader), e.g. `/lib64/ld-linux-x86-64.so.2`.
    pub interpreter: Option<String>,
    pub statically_linked: bool,
    /// Minimum OS version (Mach-O `LC_BUILD_VERSION`), e.g. `11.0`.
    pub min_os_version: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProgramSummary {
    pub format: &'static str,
    pub os: GuestOs,
    /// Set when the OS was inferred rather than stated by the file.
    pub os_note: Option<String>,
    pub slices: Vec<Slice>,
}

/// What a file turned out to be.
#[derive(Debug)]
pub enum Detected {
    /// Windows PE: full detail from `anyos-pe`.
    Pe(Box<PeImage>),
    /// ELF or Mach-O summary.
    Other(ProgramSummary),
}

pub fn detect(bytes: &[u8]) -> Result<Detected, FormatError> {
    if bytes.starts_with(b"MZ") {
        return Ok(Detected::Pe(Box::new(anyos_pe::parse(bytes)?)));
    }
    if bytes.starts_with(b"\x7fELF") {
        let elf = goblin::elf::Elf::parse(bytes).map_err(|e| FormatError::Malformed {
            format: "ELF",
            message: e.to_string(),
        })?;
        return Ok(Detected::Other(elf_summary(&elf)));
    }
    let magic = bytes
        .get(..4)
        .map(|m| u32::from_be_bytes(m.try_into().unwrap()));
    if let Some(m) = magic {
        // Mach-O (either endianness, 32/64) or a fat/universal container.
        if matches!(
            m,
            0xfeedface | 0xfeedfacf | 0xcefaedfe | 0xcffaedfe | 0xcafebabe | 0xcafebabf
        ) {
            let mach = goblin::mach::Mach::parse(bytes).map_err(|e| FormatError::Malformed {
                format: "Mach-O",
                message: e.to_string(),
            })?;
            return mach_summary(&mach);
        }
    }
    let mut m = [0u8; 8];
    let n = bytes.len().min(8);
    m[..n].copy_from_slice(&bytes[..n]);
    Err(FormatError::Unknown(u64::from_be_bytes(m)))
}

fn elf_summary(elf: &goblin::elf::Elf) -> ProgramSummary {
    use goblin::elf::header::*;
    let h = &elf.header;
    let cpu = match h.e_machine {
        EM_386 => Cpu::X86,
        EM_X86_64 => Cpu::X64,
        EM_ARM => Cpu::Arm,
        EM_AARCH64 => Cpu::Arm64,
        EM_RISCV if elf.is_64 => Cpu::RiscV64,
        other => Cpu::Other(format!("ELF machine {other}")),
    };
    let interp = elf.interpreter.map(str::to_string);
    let osabi = h.e_ident[EI_OSABI];
    let (os, os_note) = match (osabi, interp.as_deref()) {
        (ELFOSABI_FREEBSD, _) => (GuestOs::FreeBsd, None),
        (_, Some(i)) if i.starts_with("/system/bin/linker") => (GuestOs::Android, None),
        (_, Some(i)) if i.contains("ld-elf.so") => (GuestOs::FreeBsd, None),
        (_, Some(i))
            if i.contains("ld-linux") || i.contains("ld-musl") || i.contains("ld64.so") =>
        {
            (GuestOs::Linux, None)
        }
        (ELFOSABI_LINUX, _) => (GuestOs::Linux, None),
        _ => (
            GuestOs::Linux,
            Some("no OS marker in the file; assuming Linux".to_string()),
        ),
    };
    let kind = match h.e_type {
        ET_EXEC => Kind::Executable,
        // Position-independent executables are ET_DYN with an interpreter.
        ET_DYN if interp.is_some() => Kind::Executable,
        ET_DYN => Kind::SharedLibrary,
        ET_REL => Kind::Object,
        _ => Kind::Other,
    };
    ProgramSummary {
        format: "ELF",
        os,
        os_note,
        slices: vec![Slice {
            cpu,
            bits: if elf.is_64 { 64 } else { 32 },
            kind,
            entry: (elf.entry != 0).then_some(elf.entry),
            libraries: elf.libraries.iter().map(|s| s.to_string()).collect(),
            statically_linked: interp.is_none() && elf.libraries.is_empty(),
            interpreter: interp,
            min_os_version: None,
        }],
    }
}

fn mach_summary(mach: &goblin::mach::Mach) -> Result<Detected, FormatError> {
    use goblin::mach::{Mach, SingleArch};
    let mut slices = Vec::new();
    let mut platforms = Vec::new();
    let mut add = |m: &goblin::mach::MachO| {
        let (slice, platform) = mach_slice(m);
        slices.push(slice);
        platforms.push(platform);
    };
    match mach {
        Mach::Binary(m) => add(m),
        Mach::Fat(fat) => {
            for i in 0..fat.narches.min(16) {
                match fat.get(i) {
                    Ok(SingleArch::MachO(m)) => add(&m),
                    Ok(SingleArch::Archive(_)) => {}
                    Err(e) => {
                        return Err(FormatError::Malformed {
                            format: "Mach-O",
                            message: e.to_string(),
                        })
                    }
                }
            }
        }
    }
    // LC_BUILD_VERSION platform: 1 = macOS; 2,3,4,6,7.. = iOS family / simulators.
    let os = match platforms.iter().flatten().next() {
        Some(1) | None => GuestOs::MacOs,
        Some(_) => GuestOs::AppleMobile,
    };
    let os_note = platforms
        .iter()
        .all(Option::is_none)
        .then(|| "no platform marker; assuming macOS".to_string());
    Ok(Detected::Other(ProgramSummary {
        format: "Mach-O",
        os,
        os_note,
        slices,
    }))
}

fn mach_slice(m: &goblin::mach::MachO) -> (Slice, Option<u32>) {
    use goblin::mach::constants::cputype::*;
    use goblin::mach::header::*;
    let cpu = match m.header.cputype {
        CPU_TYPE_X86 => Cpu::X86,
        CPU_TYPE_X86_64 => Cpu::X64,
        CPU_TYPE_ARM => Cpu::Arm,
        CPU_TYPE_ARM64 | CPU_TYPE_ARM64_32 => Cpu::Arm64,
        other => Cpu::Other(format!("Mach-O CPU 0x{other:x}")),
    };
    let kind = match m.header.filetype {
        MH_EXECUTE => Kind::Executable,
        MH_DYLIB | MH_BUNDLE => Kind::SharedLibrary,
        MH_OBJECT => Kind::Object,
        _ => Kind::Other,
    };
    let mut platform = None;
    let mut min_os = None;
    for lc in &m.load_commands {
        match &lc.command {
            CommandVariant::BuildVersion(b) => {
                platform = Some(b.platform);
                min_os = Some(version(b.minos));
            }
            CommandVariant::VersionMinMacosx(v) => {
                platform.get_or_insert(1);
                min_os.get_or_insert(version(v.version));
            }
            _ => {}
        }
    }
    // goblin lists the image itself as "self" in libs; drop it.
    let libraries = m
        .libs
        .iter()
        .filter(|l| **l != "self")
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    let slice = Slice {
        cpu,
        bits: if m.is_64 { 64 } else { 32 },
        kind,
        entry: (m.entry != 0).then_some(m.entry),
        statically_linked: libraries.is_empty(),
        libraries,
        interpreter: None,
        min_os_version: min_os,
    };
    (slice, platform)
}

/// `xxxx.yy.zz` nibble-encoded version → `X.Y` or `X.Y.Z`.
fn version(v: u32) -> String {
    let (x, y, z) = (v >> 16, (v >> 8) & 0xff, v & 0xff);
    if z == 0 {
        format!("{x}.{y}")
    } else {
        format!("{x}.{y}.{z}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GuestMode {
    /// Built for this OS and CPU: the host runs it directly, AnyOS is not needed.
    HostNative,
    /// Same CPU, different OS: native code, AnyOS supplies the OS personality.
    NativeCpu,
    /// Different CPU: needs translation plus the OS personality.
    Translated,
    Unsupported,
}

#[derive(Debug, Clone, Serialize)]
pub struct GuestPlan {
    pub host: HostPlatform,
    pub mode: GuestMode,
    /// Index into `slices` of the architecture AnyOS would pick.
    pub slice: usize,
    /// The OS layer AnyOS must provide (empty when the host runs it directly).
    pub personality: Option<&'static str>,
    pub note: String,
}

/// Plan how a non-Windows program would run on `host`.
pub fn plan(p: &ProgramSummary, host: HostPlatform) -> GuestPlan {
    let same_os = matches!(
        (p.os, host.os),
        (GuestOs::Linux, HostOs::Linux) | (GuestOs::MacOs, HostOs::MacOs)
    );
    let cpu_match = |s: &Slice| s.cpu.host_arch() == Some(host.arch);
    let rosetta = |s: &Slice| host.is_apple_silicon() && s.cpu == Cpu::X64;
    let best = p
        .slices
        .iter()
        .position(cpu_match)
        .or_else(|| p.slices.iter().position(rosetta));
    let personality = match p.os {
        GuestOs::Linux => Some("Linux system calls and dynamic loader"),
        GuestOs::Android => Some("Android runtime (Bionic libc, ART)"),
        GuestOs::FreeBsd => Some("FreeBSD system calls"),
        GuestOs::MacOs => Some("Darwin system calls and Apple frameworks"),
        GuestOs::AppleMobile => Some("iOS frameworks (UIKit)"),
        GuestOs::Windows | GuestOs::Unknown => None,
    };
    let (mode, slice, personality, note) = match (same_os, best) {
        (true, Some(i)) if cpu_match(&p.slices[i]) => (
            GuestMode::HostNative,
            i,
            None,
            format!(
                "built for {} on {}: runs directly, AnyOS is not needed",
                p.os.display_name(),
                p.slices[i].cpu.display_name()
            ),
        ),
        (true, Some(i)) => (
            GuestMode::HostNative,
            i,
            None,
            "x86-64 macOS program on Apple Silicon: macOS runs it through Rosetta 2".to_string(),
        ),
        (false, Some(i)) if cpu_match(&p.slices[i]) => (
            GuestMode::NativeCpu,
            i,
            personality,
            format!(
                "{} code runs on this CPU; AnyOS must provide the {} layer (planned)",
                p.slices[i].cpu.display_name(),
                p.os.display_name()
            ),
        ),
        _ if p.os == GuestOs::Unknown || p.slices.is_empty() => (
            GuestMode::Unsupported,
            0,
            None,
            "unknown target OS".to_string(),
        ),
        _ => (
            GuestMode::Translated,
            best.unwrap_or(0),
            personality,
            format!(
                "{} code needs translation on {}; AnyOS must also provide the {} layer (planned)",
                p.slices[best.unwrap_or(0)].cpu.display_name(),
                host.display_name(),
                p.os.display_name()
            ),
        ),
    };
    GuestPlan {
        host,
        mode,
        slice,
        personality,
        note,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_decode() {
        assert_eq!(version(0x000b_0000), "11.0");
        assert_eq!(version(0x000a_0f01), "10.15.1");
    }

    #[test]
    fn junk_is_unknown() {
        assert!(matches!(
            detect(b"#!/bin/sh\necho hi\n"),
            Err(FormatError::Unknown(_))
        ));
        assert!(matches!(detect(b""), Err(FormatError::Unknown(0))));
        assert!(detect(b"\x7fELF\x02\x01").is_err());
        assert!(detect(&[0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 9]).is_err());
    }
}
