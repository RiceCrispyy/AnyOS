//! # anyos-loader
//!
//! Prepares Windows PE images in a guest address space the way the Windows loader
//! would, without executing anything:
//!
//! 1. **Map** headers and sections into an [`AddressSpace`], at the preferred base
//!    when it is free and otherwise at a new base.
//! 2. **Relocate** by applying base relocations for the difference.
//! 3. **Bind imports**: each IAT slot gets the export address from another loaded
//!    guest module, or a [`CallGate`] stub for APIs AnyOS must implement itself.
//! 4. **Protect** each section according to its characteristics.
//! 5. Record the entry point, TLS slot and TLS callbacks for the execution layer.
//!
//! Mapping is separate from binding so that DLLs that import from each other
//! (cycles are legal on Windows) can all be mapped before any of them is bound.
//! The loader never implements Windows APIs itself.

mod gate;
mod image;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyos_memory::{AddressSpace, MemoryError, Protection};
use anyos_pe::catalog::{classify, ApiFamily};
use anyos_pe::{ExportTarget, ImportName, Machine, PeError, PeFormat, PeImage};
use serde::Serialize;

pub use gate::{CallGate, GateEntry, STUB_SIZE};

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    Pe(#[from] PeError),
    #[error("guest memory: {0}")]
    Memory(#[from] MemoryError),
    #[error("the {0} directory is damaged")]
    BrokenDirectory(String),
    #[error("cannot place image at its preferred base 0x{0:x} and it has no relocations")]
    NotRelocatable(u64),
    #[error("unsupported base relocation type {kind} at RVA 0x{rva:x}")]
    UnsupportedRelocation { kind: u8, rva: u32 },
    #[error("relocation at RVA 0x{0:x} falls outside the image")]
    RelocationOutOfImage(u32),
    #[error("{module} is {found} but the process is {expected}")]
    ArchitectureMismatch {
        module: String,
        found: String,
        expected: String,
    },
    #[error("forwarder chain too deep while resolving {0}")]
    ForwarderLoop(String),
    #[error("{0} is already loaded")]
    AlreadyLoaded(String),
}

/// Where to place an image.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BasePolicy {
    /// The image's preferred base, or any free address if that is taken.
    #[default]
    Preferred,
    /// Always relocate (useful to exercise relocation paths).
    ForceRelocation,
    /// A specific address.
    At(u64),
}

#[derive(Debug, Clone, Default)]
pub struct LoadOptions {
    pub base: BasePolicy,
    /// Directories searched for application DLLs (not Windows system DLLs).
    pub search_dirs: Vec<PathBuf>,
    /// Load application DLLs found in `search_dirs`.
    pub load_dependencies: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct MappedSection {
    pub name: String,
    pub address: u64,
    pub size: u64,
    pub protection: Protection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Binding {
    /// Bound to an export of another guest module.
    Module { module: String },
    /// Bound to a call-gate stub (an API the runtime provides).
    CallGate { stub: u32 },
}

#[derive(Debug, Clone, Serialize)]
pub struct BoundImport {
    pub dll: String,
    pub symbol: ImportName,
    pub iat_address: u64,
    pub target: u64,
    pub binding: Binding,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoadedTls {
    /// Process-wide TLS slot index written to the image's `AddressOfIndex`.
    pub index: u32,
    pub template_start: u64,
    pub template_end: u64,
    pub zero_fill: u32,
    /// Relocated callback addresses, in call order.
    pub callbacks: Vec<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoadedModule {
    /// File name as loaded (e.g. `app.exe`).
    pub name: String,
    pub path: Option<PathBuf>,
    pub base: u64,
    pub size: u64,
    pub preferred_base: u64,
    pub relocations_applied: usize,
    pub entry_point: Option<u64>,
    pub sections: Vec<MappedSection>,
    pub imports: Vec<BoundImport>,
    /// Delay-load DLLs; their IAT slots keep pointing at the image's own helper
    /// thunks until the guest calls them.
    pub delay_load_dlls: Vec<String>,
    pub tls: Option<LoadedTls>,
    pub stack_reserve: u64,
    pub stack_commit: u64,
    #[serde(skip)]
    pub image: PeImage,
    #[serde(skip)]
    exports_by_name: HashMap<String, usize>,
    bound: bool,
}

impl LoadedModule {
    pub fn rebased(&self) -> bool {
        self.base != self.preferred_base
    }

    /// Resolve an export of this module to either an address or a forwarder string.
    pub fn find_export(&self, symbol: &ImportName) -> Option<&ExportTarget> {
        let exports = self.image.exports.as_ref()?;
        let idx = match symbol {
            ImportName::Name { name, .. } => *self.exports_by_name.get(name)?,
            ImportName::Ordinal { ordinal } => exports
                .exports
                .iter()
                .position(|e| e.ordinal == *ordinal as u32)?,
        };
        Some(&exports.exports[idx].target)
    }
}

/// A guest process being assembled: its address space, modules and call gate.
#[derive(Debug)]
pub struct Process {
    pub memory: AddressSpace,
    pub modules: Vec<LoadedModule>,
    pub call_gate: CallGate,
    pub machine: Machine,
    pub format: PeFormat,
    next_tls_index: u32,
}

impl Process {
    /// An empty process whose architecture matches `main` (the executable).
    pub fn for_image(main: &PeImage) -> Self {
        let laa = main.characteristics & anyos_pe::consts::IMAGE_FILE_LARGE_ADDRESS_AWARE != 0;
        Process {
            memory: AddressSpace::windows_user(main.format.pointer_size(), laa),
            modules: Vec::new(),
            call_gate: CallGate::default(),
            machine: main.machine,
            format: main.format,
            next_tls_index: 0,
        }
    }

    /// Read `path`, create a process for it, map it (and, when asked, its
    /// application DLLs), then bind every module's imports.
    pub fn load_executable(path: &Path, opts: &LoadOptions) -> Result<Process, LoadError> {
        let bytes = read(path)?;
        let image = anyos_pe::parse(&bytes)?;
        let mut process = Process::for_image(&image);
        let name = file_name(path);
        process.map_image(&name, Some(path.to_path_buf()), &bytes, image, opts.base)?;
        if opts.load_dependencies {
            process.load_dependencies(opts)?;
        }
        process.bind_all()?;
        Ok(process)
    }

    pub fn module(&self, name: &str) -> Option<&LoadedModule> {
        self.module_index(name).map(|i| &self.modules[i])
    }

    fn module_index(&self, name: &str) -> Option<usize> {
        let want = normalize_dll(name);
        self.modules
            .iter()
            .position(|m| normalize_dll(&m.name) == want)
    }

    /// Map and relocate one image. Imports are bound later by [`Process::bind_all`].
    pub fn map_image(
        &mut self,
        name: &str,
        path: Option<PathBuf>,
        bytes: &[u8],
        image: PeImage,
        base: BasePolicy,
    ) -> Result<usize, LoadError> {
        if self.module_index(name).is_some() {
            return Err(LoadError::AlreadyLoaded(name.to_string()));
        }
        if image.machine != self.machine || image.format != self.format {
            return Err(LoadError::ArchitectureMismatch {
                module: name.to_string(),
                found: format!(
                    "{} {}",
                    image.format.display_name(),
                    image.machine.display_name()
                ),
                expected: format!(
                    "{} {}",
                    self.format.display_name(),
                    self.machine.display_name()
                ),
            });
        }
        let module = image::map(self, name, path, bytes, image, base)?;
        self.modules.push(module);
        Ok(self.modules.len() - 1)
    }

    /// Load application DLLs imported (directly or transitively) by mapped modules
    /// from `opts.search_dirs`. System DLLs are left to the call gate.
    pub fn load_dependencies(&mut self, opts: &LoadOptions) -> Result<(), LoadError> {
        let mut i = 0;
        while i < self.modules.len() {
            let wanted: Vec<String> = self.modules[i]
                .image
                .imports
                .iter()
                .filter(|m| !m.delay_load && classify(&m.dll) == ApiFamily::Application)
                .map(|m| m.dll.clone())
                .collect();
            for dll in wanted {
                if self.module_index(&dll).is_some() {
                    continue;
                }
                if let Some(path) = find_in(&opts.search_dirs, &dll) {
                    let bytes = read(&path)?;
                    let image = anyos_pe::parse(&bytes)?;
                    self.map_image(&dll, Some(path), &bytes, image, BasePolicy::Preferred)?;
                }
            }
            i += 1;
        }
        Ok(())
    }

    /// Bind imports of every module not yet bound, assign TLS slots, then apply
    /// final page protections.
    pub fn bind_all(&mut self) -> Result<(), LoadError> {
        for i in 0..self.modules.len() {
            if self.modules[i].bound {
                continue;
            }
            let ptr = self.format.pointer_size();
            let wanted: Vec<(String, ImportName, u64)> = self.modules[i]
                .image
                .imports
                .iter()
                .filter(|m| !m.delay_load)
                .flat_map(|m| {
                    let base = self.modules[i].base;
                    m.symbols
                        .iter()
                        .map(move |s| (m.dll.clone(), s.name.clone(), base + s.iat_rva as u64))
                })
                .collect();
            let mut bound = Vec::with_capacity(wanted.len());
            for (dll, symbol, iat_address) in wanted {
                let (target, binding) = self.resolve(&dll, &symbol, 0)?;
                self.memory.write_ptr_raw(iat_address, ptr, target)?;
                bound.push(BoundImport {
                    dll,
                    symbol,
                    iat_address,
                    target,
                    binding,
                });
            }
            self.modules[i].imports = bound;
            image::assign_tls(self, i)?;
            image::protect(self, i)?;
            self.modules[i].bound = true;
        }
        Ok(())
    }

    /// Resolve `dll!symbol` to a guest address.
    pub fn resolve(
        &mut self,
        dll: &str,
        symbol: &ImportName,
        depth: u32,
    ) -> Result<(u64, Binding), LoadError> {
        if depth > 16 {
            return Err(LoadError::ForwarderLoop(format!("{dll}!{symbol}")));
        }
        if let Some(idx) = self.module_index(dll) {
            let m = &self.modules[idx];
            match m.find_export(symbol).cloned() {
                Some(ExportTarget::Rva { rva }) => {
                    return Ok((
                        m.base + rva as u64,
                        Binding::Module {
                            module: m.name.clone(),
                        },
                    ));
                }
                Some(ExportTarget::Forwarder { target }) => {
                    if let Some((fdll, fsym)) = parse_forwarder(&target) {
                        return self.resolve(&fdll, &fsym, depth + 1);
                    }
                }
                None => {} // fall through: a missing export becomes a gate stub the backend reports
            }
        }
        let addr = self
            .call_gate
            .stub_for(&mut self.memory, self.machine, dll, symbol)?;
        let stub = self
            .call_gate
            .lookup(addr)
            .map(|e| e.id)
            .unwrap_or_default();
        Ok((addr, Binding::CallGate { stub }))
    }
}

/// `NTDLL.RtlAllocateHeap` → (`NTDLL.dll`, name); `NTDLL.#12` → ordinal 12.
fn parse_forwarder(target: &str) -> Option<(String, ImportName)> {
    let (dll, sym) = target.rsplit_once('.')?;
    let dll = format!("{dll}.dll");
    let symbol = match sym.strip_prefix('#') {
        Some(n) => ImportName::Ordinal {
            ordinal: n.parse().ok()?,
        },
        None => ImportName::Name {
            name: sym.to_string(),
            hint: 0,
        },
    };
    Some((dll, symbol))
}

/// Lower-case and drop a trailing `.dll`, matching how Windows compares module names.
fn normalize_dll(name: &str) -> String {
    let n = name.to_ascii_lowercase();
    n.strip_suffix(".dll").map(str::to_string).unwrap_or(n)
}

fn find_in(dirs: &[PathBuf], dll: &str) -> Option<PathBuf> {
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for e in entries.flatten() {
            if e.file_name().to_string_lossy().eq_ignore_ascii_case(dll) && e.path().is_file() {
                return Some(e.path());
            }
        }
    }
    None
}

fn read(path: &Path) -> Result<Vec<u8>, LoadError> {
    std::fs::read(path).map_err(|source| LoadError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwarders_parse() {
        assert_eq!(
            parse_forwarder("NTDLL.RtlAllocateHeap"),
            Some((
                "NTDLL.dll".into(),
                ImportName::Name {
                    name: "RtlAllocateHeap".into(),
                    hint: 0
                }
            ))
        );
        assert_eq!(
            parse_forwarder("api-ms-win-core-x.#5"),
            Some((
                "api-ms-win-core-x.dll".into(),
                ImportName::Ordinal { ordinal: 5 }
            ))
        );
        assert_eq!(parse_forwarder("nodot"), None);
    }

    #[test]
    fn module_names_compare_like_windows() {
        assert_eq!(normalize_dll("KERNEL32.DLL"), normalize_dll("kernel32"));
    }
}
