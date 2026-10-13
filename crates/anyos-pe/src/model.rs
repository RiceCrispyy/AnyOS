//! Owned, serializable description of a PE image.
//!
//! Everything here is plain data copied out of the file, so later stages (the
//! loader, compatibility reports, the CLI) never hold borrows into untrusted bytes.

use serde::Serialize;

/// PE optional-header flavour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PeFormat {
    /// 32-bit images (`IMAGE_NT_OPTIONAL_HDR32_MAGIC`).
    Pe32,
    /// 64-bit images (`IMAGE_NT_OPTIONAL_HDR64_MAGIC`).
    #[serde(rename = "Pe32Plus")]
    Pe32Plus,
}

impl PeFormat {
    pub fn pointer_size(self) -> usize {
        match self {
            PeFormat::Pe32 => 4,
            PeFormat::Pe32Plus => 8,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            PeFormat::Pe32 => "PE32",
            PeFormat::Pe32Plus => "PE32+",
        }
    }
}

/// Target CPU from `IMAGE_FILE_HEADER.Machine`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Machine {
    X86,
    X64,
    Arm64,
    /// ARM64EC / ARM64X hybrid images (x64-compatible ABI on ARM64).
    Arm64Ec,
    /// 32-bit ARM (Thumb-2), used by old Windows RT binaries.
    ArmNt,
    Unknown(u16),
}

impl Machine {
    pub fn from_raw(raw: u16) -> Self {
        match raw {
            0x014c => Machine::X86,
            0x8664 => Machine::X64,
            0xaa64 => Machine::Arm64,
            0xa641 => Machine::Arm64Ec,
            0x01c4 => Machine::ArmNt,
            other => Machine::Unknown(other),
        }
    }

    pub fn display_name(self) -> String {
        match self {
            Machine::X86 => "x86".into(),
            Machine::X64 => "x86-64".into(),
            Machine::Arm64 => "ARM64".into(),
            Machine::Arm64Ec => "ARM64EC".into(),
            Machine::ArmNt => "ARM (Thumb-2)".into(),
            Machine::Unknown(raw) => format!("unknown (0x{raw:04x})"),
        }
    }
}

/// `IMAGE_OPTIONAL_HEADER.Subsystem`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Subsystem {
    Unknown,
    Native,
    WindowsGui,
    WindowsConsole,
    Os2Console,
    PosixConsole,
    WindowsCeGui,
    EfiApplication,
    EfiBootServiceDriver,
    EfiRuntimeDriver,
    EfiRom,
    Xbox,
    WindowsBootApplication,
    Other(u16),
}

impl Subsystem {
    pub fn from_raw(raw: u16) -> Self {
        match raw {
            0 => Subsystem::Unknown,
            1 => Subsystem::Native,
            2 => Subsystem::WindowsGui,
            3 => Subsystem::WindowsConsole,
            5 => Subsystem::Os2Console,
            7 => Subsystem::PosixConsole,
            9 => Subsystem::WindowsCeGui,
            10 => Subsystem::EfiApplication,
            11 => Subsystem::EfiBootServiceDriver,
            12 => Subsystem::EfiRuntimeDriver,
            13 => Subsystem::EfiRom,
            14 => Subsystem::Xbox,
            16 => Subsystem::WindowsBootApplication,
            other => Subsystem::Other(other),
        }
    }

    pub fn display_name(self) -> String {
        match self {
            Subsystem::Unknown => "Unknown".into(),
            Subsystem::Native => "Native (kernel/driver)".into(),
            Subsystem::WindowsGui => "Windows GUI".into(),
            Subsystem::WindowsConsole => "Windows console".into(),
            Subsystem::Os2Console => "OS/2 console".into(),
            Subsystem::PosixConsole => "POSIX console".into(),
            Subsystem::WindowsCeGui => "Windows CE GUI".into(),
            Subsystem::EfiApplication => "EFI application".into(),
            Subsystem::EfiBootServiceDriver => "EFI boot service driver".into(),
            Subsystem::EfiRuntimeDriver => "EFI runtime driver".into(),
            Subsystem::EfiRom => "EFI ROM".into(),
            Subsystem::Xbox => "Xbox".into(),
            Subsystem::WindowsBootApplication => "Windows boot application".into(),
            Subsystem::Other(raw) => format!("other ({raw})"),
        }
    }
}

/// Memory protection implied by section characteristics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct SectionAccess {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

impl SectionAccess {
    pub fn from_characteristics(c: u32) -> Self {
        SectionAccess {
            read: c & crate::consts::IMAGE_SCN_MEM_READ != 0,
            write: c & crate::consts::IMAGE_SCN_MEM_WRITE != 0,
            execute: c & crate::consts::IMAGE_SCN_MEM_EXECUTE != 0,
        }
    }

    /// `rwx` style string, e.g. `r-x`.
    pub fn flags(self) -> String {
        format!(
            "{}{}{}",
            if self.read { 'r' } else { '-' },
            if self.write { 'w' } else { '-' },
            if self.execute { 'x' } else { '-' }
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Section {
    pub name: String,
    pub virtual_address: u32,
    pub virtual_size: u32,
    pub raw_offset: u32,
    pub raw_size: u32,
    pub characteristics: u32,
    pub access: SectionAccess,
}

impl Section {
    /// Size the section occupies once mapped (Windows uses `VirtualSize`, or the raw
    /// size when `VirtualSize` is zero).
    pub fn mapped_size(&self) -> u32 {
        if self.virtual_size == 0 {
            self.raw_size
        } else {
            self.virtual_size
        }
    }
}

/// How an import is requested from its DLL.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ImportName {
    /// Imported by name, with the loader hint (index into the DLL's name table).
    Name { name: String, hint: u16 },
    /// Imported by ordinal only.
    Ordinal { ordinal: u16 },
}

impl std::fmt::Display for ImportName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportName::Name { name, .. } => f.write_str(name),
            ImportName::Ordinal { ordinal } => write!(f, "#{ordinal}"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportedSymbol {
    pub name: ImportName,
    /// RVA of the import address table slot the loader must fill in.
    pub iat_rva: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportedModule {
    /// DLL name exactly as written in the image (case preserved).
    pub dll: String,
    /// True for entries from the delay-load import directory.
    pub delay_load: bool,
    pub symbols: Vec<ImportedSymbol>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExportTarget {
    /// Code or data inside this image.
    Rva { rva: u32 },
    /// Forwarded to another DLL, e.g. `NTDLL.RtlAllocateHeap` or `NTDLL.#12`.
    Forwarder { target: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct Export {
    pub ordinal: u32,
    pub name: Option<String>,
    pub target: ExportTarget,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExportTable {
    /// The DLL's own name from the export directory, if present.
    pub dll_name: Option<String>,
    pub ordinal_base: u32,
    pub exports: Vec<Export>,
}

/// One base-relocation fixup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct BaseRelocation {
    pub rva: u32,
    pub kind: RelocationKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RelocationKind {
    /// Add the high 16 bits of the delta to the 16-bit field.
    High,
    /// Add the low 16 bits of the delta to the 16-bit field.
    Low,
    /// Add the delta to a 32-bit field.
    HighLow,
    /// High 16 bits with the low half carried in `adjust` (the following entry).
    HighAdj { adjust: u16 },
    /// Add the delta to a 64-bit field.
    Dir64,
    /// ARM `MOVW`/`MOVT` pair (type 5 on ARM images).
    ArmMov32,
    /// Thumb-2 `MOVW`/`MOVT` pair (type 7).
    ThumbMov32,
    /// Any type this loader does not apply; kept so callers can reject the image.
    Unsupported(u8),
}

#[derive(Debug, Clone, Serialize)]
pub struct TlsDirectory {
    /// Virtual addresses as stored in the image (relative to the preferred base).
    pub raw_data_start_va: u64,
    pub raw_data_end_va: u64,
    pub index_va: u64,
    pub callbacks_va: u64,
    pub zero_fill_size: u32,
    pub characteristics: u32,
    /// Callback VAs (preferred base), in call order.
    pub callbacks: Vec<u64>,
}

/// Information from the CLR (COM descriptor) header and metadata root.
#[derive(Debug, Clone, Serialize)]
pub struct ClrInfo {
    pub runtime_major: u16,
    pub runtime_minor: u16,
    pub flags: u32,
    /// Metadata version string such as `v4.0.30319`.
    pub metadata_version: Option<String>,
    pub il_only: bool,
    pub requires_32bit: bool,
    pub prefers_32bit: bool,
    pub strong_name_signed: bool,
    pub native_entry_point: bool,
    pub entry_point_token: u32,
    /// Ready-to-run (or other) managed native code header present.
    pub has_native_code_header: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct DataDirectory {
    pub rva: u32,
    pub size: u32,
}

/// Everything the inspector learned about an image.
#[derive(Debug, Clone, Serialize)]
pub struct PeImage {
    pub format: PeFormat,
    pub machine: Machine,
    pub machine_raw: u16,
    pub is_dll: bool,
    pub subsystem: Subsystem,
    pub file_size: u64,
    pub timestamp: u32,
    pub characteristics: u16,
    pub dll_characteristics: u16,
    pub linker_version: (u8, u8),
    pub os_version: (u16, u16),
    pub subsystem_version: (u16, u16),
    pub image_base: u64,
    pub size_of_image: u32,
    pub size_of_headers: u32,
    pub section_alignment: u32,
    pub file_alignment: u32,
    /// Entry point RVA, or `None` when the image has none (resource-only DLLs).
    pub entry_point_rva: Option<u32>,
    pub stack_reserve: u64,
    pub stack_commit: u64,
    pub heap_reserve: u64,
    pub heap_commit: u64,
    pub sections: Vec<Section>,
    /// The 16 standard data directories (zeroed when absent).
    pub data_directories: Vec<DataDirectory>,
    pub imports: Vec<ImportedModule>,
    pub exports: Option<ExportTable>,
    pub relocations: Vec<BaseRelocation>,
    pub tls: Option<TlsDirectory>,
    pub clr: Option<ClrInfo>,
    pub has_resources: bool,
    pub has_exception_table: bool,
    pub has_load_config: bool,
    pub has_signature: bool,
    pub has_debug_info: bool,
    /// Bytes after the last section's raw data (installers often append payloads).
    pub overlay_size: u64,
    /// Data directories that were present but could not be parsed (e.g. `"import"`).
    /// The inspector still reports the rest; the loader refuses such images.
    pub broken_directories: Vec<String>,
    /// Non-fatal problems found while parsing.
    pub warnings: Vec<String>,
}

impl PeImage {
    pub fn is_managed(&self) -> bool {
        self.clr.is_some()
    }

    pub fn dynamic_base(&self) -> bool {
        self.dll_characteristics & crate::consts::IMAGE_DLLCHARACTERISTICS_DYNAMIC_BASE != 0
    }

    pub fn relocations_stripped(&self) -> bool {
        self.characteristics & crate::consts::IMAGE_FILE_RELOCS_STRIPPED != 0
    }

    pub fn import_count(&self) -> usize {
        self.imports.iter().map(|m| m.symbols.len()).sum()
    }

    /// Section containing `rva` in its mapped range.
    pub fn section_for_rva(&self, rva: u32) -> Option<&Section> {
        self.sections.iter().find(|s| {
            rva >= s.virtual_address
                && (rva as u64) < s.virtual_address as u64 + s.mapped_size() as u64
        })
    }
}
