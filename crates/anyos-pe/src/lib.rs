//! # anyos-pe
//!
//! Defensive inspection of Windows PE/COFF images for the AnyOS runtime.
//!
//! * Parsing never executes code and never panics on malformed input; every
//!   offset is bounds-checked and every table is capped by [`Limits`].
//! * goblin handles the DOS/NT headers and section table. Data directories
//!   (imports, delay imports, exports, base relocations, TLS, CLR) are parsed here
//!   so their semantics are explicit and tested.
//! * The result, [`PeImage`], is owned data that the loader and the CLI share.
//!
//! ```no_run
//! let bytes = std::fs::read("app.exe").unwrap();
//! let image = anyos_pe::parse(&bytes).unwrap();
//! println!("{} {}", image.format.display_name(), image.machine.display_name());
//! ```

pub mod catalog;
pub mod consts;
mod model;
mod parse;
pub mod report;
mod view;

pub use model::*;

/// Why an input could not be inspected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PeError {
    #[error("not a PE image: {0}")]
    NotPe(String),
    #[error("file is truncated: {0}")]
    Truncated(String),
    #[error("malformed PE image: {0}")]
    Malformed(String),
    #[error("unsupported PE image: {0}")]
    Unsupported(String),
    #[error("resource limit exceeded: {0}")]
    LimitExceeded(String),
}

/// Upper bounds applied while parsing untrusted files.
#[derive(Debug, Clone)]
pub struct Limits {
    pub max_file_size: u64,
    pub max_image_size: u64,
    pub max_sections: usize,
    pub max_import_modules: usize,
    pub max_imports_per_module: usize,
    pub max_exports: usize,
    pub max_relocations: usize,
    pub max_tls_callbacks: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_file_size: 4 << 30,
            max_image_size: 3 << 30,
            max_sections: 96, // the Windows loader's own limit
            max_import_modules: 4096,
            max_imports_per_module: 65_536,
            max_exports: 65_536,
            max_relocations: 16 << 20,
            max_tls_callbacks: 1024,
        }
    }
}

/// Parse a PE image with default [`Limits`].
pub fn parse(bytes: &[u8]) -> Result<PeImage, PeError> {
    parse::parse(bytes, &Limits::default())
}

/// Parse a PE image with caller-supplied limits.
pub fn parse_with_limits(bytes: &[u8], limits: &Limits) -> Result<PeImage, PeError> {
    parse::parse(bytes, limits)
}
