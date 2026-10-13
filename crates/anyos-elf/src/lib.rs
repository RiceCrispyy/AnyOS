//! # anyos-elf
//!
//! Loads ELF executables into a guest address space, preparing them for
//! execution by AnyOS's execution backends.
//!
//! Supports x86-64 and AArch64 static and dynamically-linked ELF binaries.
//! Dynamic linking (program interpreter path recorded, but the interpreter
//! itself is not loaded here — that's the loader's job at run time).
//!
//! Maps each `PT_LOAD` segment into the [`AddressSpace`] model, applies page
//! protections, and computes the auxiliary-vector entries the program's libc
//! startup code expects on the initial stack.
//!
//! Nothing here executes guest code.

use anyos_memory::{round_down, round_up, AddressSpace, MemoryError, Protection, PAGE_SIZE};
use goblin::elf::{
    program_header::{self, ProgramHeader},
    Elf,
};
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum ElfError {
    #[error("goblin parse error: {0}")]
    Parse(#[from] goblin::error::Error),
    #[error("not an ELF file")]
    NotElf,
    #[error("not a supported ELF type (must be ET_EXEC or ET_DYN)")]
    UnsupportedType,
    #[error("unsupported ELF class (must be ELFCLASS64)")]
    NotElf64,
    #[error("unsupported architecture e_machine=0x{0:x}")]
    UnsupportedArch(u16),
    #[error("PT_LOAD segment at vaddr 0x{vaddr:x} is not page-aligned")]
    UnalignedSegment { vaddr: u64 },
    #[error("guest memory: {0}")]
    Memory(#[from] MemoryError),
    #[error("ELF is too large or has overlapping segments")]
    TooLarge,
}

/// CPU the ELF was compiled for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ElfArch {
    X86_64,
    Aarch64,
}

impl ElfArch {
    pub fn display_name(self) -> &'static str {
        match self {
            ElfArch::X86_64 => "x86-64",
            ElfArch::Aarch64 => "AArch64",
        }
    }
}

/// Whether the ELF binary links to a dynamic interpreter.
#[derive(Debug, Clone, Serialize)]
pub enum Linkage {
    /// Statically linked: no dynamic loader required.
    Static,
    /// Dynamically linked: the given path names the ELF interpreter (ld.so).
    Dynamic { interpreter: String },
}

/// The result of loading an ELF into a guest [`AddressSpace`].
#[derive(Debug, Serialize)]
pub struct LoadedElf {
    /// Virtual address of the program entry point (possibly adjusted for PIE).
    pub entry: u64,
    /// Actual base address the image was mapped to (0 for fixed, ASLR base for PIE).
    pub load_base: u64,
    /// Virtual address where the program header table was mapped (for aux vector AT_PHDR).
    pub phdr_vaddr: u64,
    /// Number of program headers (for aux vector AT_PHNUM).
    pub phdr_count: u16,
    /// Size of one program header entry (for aux vector AT_PHENT).
    pub phdr_entry_size: u16,
    /// Architecture.
    pub arch: ElfArch,
    /// Linkage kind.
    pub linkage: Linkage,
    /// Set when the ELF is position-independent (ET_DYN).
    pub is_pie: bool,
}

/// Parsed ELF properties useful for planning execution without loading.
#[derive(Debug, Clone, Serialize)]
pub struct ElfSummary {
    pub arch: ElfArch,
    pub linkage: Linkage,
    pub is_pie: bool,
    pub entry: u64,
    pub shared_libraries: Vec<String>,
}

/// Parse an ELF binary and return a summary without loading any segments.
pub fn summarise(bytes: &[u8]) -> Result<ElfSummary, ElfError> {
    let elf = parse(bytes)?;
    let arch = arch_of(&elf)?;
    let interp = interpreter_of(&elf, bytes);
    let linkage = match interp {
        Some(path) => Linkage::Dynamic { interpreter: path },
        None => Linkage::Static,
    };
    let shared_libraries = elf
        .libraries
        .iter()
        .map(|s| s.to_string())
        .collect();
    Ok(ElfSummary {
        arch,
        linkage,
        is_pie: elf.header.e_type == goblin::elf::header::ET_DYN,
        entry: elf.header.e_entry,
        shared_libraries,
    })
}

/// Load all `PT_LOAD` segments of an ELF into `space` and return the layout.
///
/// For PIE (`ET_DYN`) executables, the image is mapped at a free address
/// chosen by `space.map_anywhere`. For fixed (`ET_EXEC`) executables the
/// preferred vaddr is used and an error is returned if it is already occupied.
///
/// Handles non-page-aligned vaddrs correctly: the ELF spec only requires
/// `vaddr ≡ file_offset (mod page_align)`, not that vaddr be a multiple of
/// the page size. Each segment is mapped starting at `floor(vaddr, PAGE_SIZE)`.
pub fn load(space: &mut AddressSpace, bytes: &[u8]) -> Result<LoadedElf, ElfError> {
    let elf = parse(bytes)?;
    let arch = arch_of(&elf)?;
    let is_pie = elf.header.e_type == goblin::elf::header::ET_DYN;

    // Collect PT_LOAD segments with non-zero memory size.
    let loads: Vec<&ProgramHeader> = elf
        .program_headers
        .iter()
        .filter(|ph| ph.p_type == program_header::PT_LOAD && ph.p_memsz > 0)
        .collect();

    if loads.is_empty() {
        return Err(ElfError::TooLarge);
    }

    // The minimum page-aligned vaddr across all segments.
    let min_page = loads
        .iter()
        .map(|ph| round_down(ph.p_vaddr, PAGE_SIZE))
        .min()
        .unwrap();
    // The maximum page-rounded end.
    let max_end = loads
        .iter()
        .map(|ph| round_up(ph.p_vaddr + ph.p_memsz, PAGE_SIZE))
        .max()
        .unwrap();
    let image_size = max_end - min_page;

    // For PIE: place the whole image anywhere free and record load_bias.
    // For EXEC: the vaddrs are fixed; min_page is the required base.
    let load_bias: u64 = if is_pie {
        let base = space.map_anywhere(image_size, Protection::RW, "elf image", None)?;
        base - min_page
    } else {
        space.map(min_page, image_size, Protection::RW, "elf image")?;
        0
    };

    // Populate each segment.
    for ph in &loads {
        let map_vaddr = load_bias + round_down(ph.p_vaddr, PAGE_SIZE);
        let vaddr_offset = (ph.p_vaddr % PAGE_SIZE) as usize;
        let file_off = ph.p_offset as usize;
        let file_len = ph.p_filesz as usize;
        let slice = bytes
            .get(file_off..file_off + file_len)
            .ok_or(ElfError::TooLarge)?;
        // Write file bytes at their actual (possibly unaligned) vaddr.
        space.write_raw(map_vaddr + vaddr_offset as u64, slice)?;
        // Apply page-granular protection to the entire mapped page range.
        let map_size = round_up(vaddr_offset as u64 + ph.p_memsz, PAGE_SIZE);
        let prot = flags_to_prot(ph.p_flags);
        space.protect(map_vaddr, map_size, prot)?;
    }

    let entry = load_bias + elf.header.e_entry;

    // AT_PHDR: virtual address of the ELF program header table.
    // e_phoff is a file offset; find which PT_LOAD contains it and compute its vaddr.
    let phdr_vaddr = {
        let phoff = elf.header.e_phoff;
        loads
            .iter()
            .find(|ph| {
                phoff >= ph.p_offset && phoff < ph.p_offset + ph.p_filesz
            })
            .map(|ph| load_bias + ph.p_vaddr + (phoff - ph.p_offset))
            .unwrap_or(load_bias + phoff)
    };

    let interp = interpreter_of(&elf, bytes);
    let linkage = match interp {
        Some(path) => Linkage::Dynamic { interpreter: path },
        None => Linkage::Static,
    };
    // load_base = where the first page was mapped.
    let load_base = min_page + load_bias;

    Ok(LoadedElf {
        entry,
        load_base,
        phdr_vaddr,
        phdr_count: elf.header.e_phnum,
        phdr_entry_size: elf.header.e_phentsize,
        arch,
        linkage,
        is_pie,
    })
}

// ── helpers ────────────────────────────────────────────────────────────────

fn parse(bytes: &[u8]) -> Result<Elf<'_>, ElfError> {
    if bytes.len() < 4 || &bytes[..4] != b"\x7fELF" {
        return Err(ElfError::NotElf);
    }
    let elf = Elf::parse(bytes)?;
    if elf.header.e_ident[goblin::elf::header::EI_CLASS]
        != goblin::elf::header::ELFCLASS64
    {
        return Err(ElfError::NotElf64);
    }
    match elf.header.e_type {
        goblin::elf::header::ET_EXEC | goblin::elf::header::ET_DYN => {}
        _ => return Err(ElfError::UnsupportedType),
    }
    Ok(elf)
}

fn arch_of(elf: &Elf<'_>) -> Result<ElfArch, ElfError> {
    match elf.header.e_machine {
        goblin::elf::header::EM_X86_64 => Ok(ElfArch::X86_64),
        goblin::elf::header::EM_AARCH64 => Ok(ElfArch::Aarch64),
        m => Err(ElfError::UnsupportedArch(m)),
    }
}

fn interpreter_of(elf: &Elf<'_>, bytes: &[u8]) -> Option<String> {
    let ph = elf
        .program_headers
        .iter()
        .find(|ph| ph.p_type == program_header::PT_INTERP)?;
    let start = ph.p_offset as usize;
    let end = (ph.p_offset + ph.p_filesz) as usize;
    let s = bytes.get(start..end)?;
    // Strip the NUL terminator.
    let s = s.strip_suffix(b"\0").unwrap_or(s);
    Some(String::from_utf8_lossy(s).into_owned())
}

fn flags_to_prot(flags: u32) -> Protection {
    use goblin::elf::program_header::{PF_R, PF_W, PF_X};
    let r = flags & PF_R != 0;
    let w = flags & PF_W != 0;
    let x = flags & PF_X != 0;
    Protection { read: r, write: w, execute: x }
}
