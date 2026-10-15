//! # anyos-win32
//!
//! The Windows personality: everything between "the loader has mapped an
//! executable" and "the program has exited".
//!
//! 1. **Load** the executable and its application DLLs with `anyos-loader`.
//!    Windows APIs are bound to call-gate stubs.
//! 2. **Set up the thread**: a stack, a TEB (reached through `gs` on x64 and
//!    `fs` on x86), a PEB, the TLS slots and the command line.
//! 3. **Run** DLL entry points, TLS callbacks and finally the program's entry
//!    point on an [`ExecutionBackend`].
//! 4. **Dispatch**: when the backend stops on a call-gate stub, look up which
//!    API it is, read the arguments with the right calling convention, run the
//!    Rust implementation, write the result and return to the caller.
//!
//! An API without an implementation stops the run with
//! [`RunError::UnimplementedApi`] naming it, so missing coverage shows up by name.

mod api;
mod console;
mod file;
mod heap;
mod locale;
mod memory;
mod module;
mod objects;
mod process;
mod registry;
mod resource;
mod seh;
mod strings;
mod sync;
mod sysinfo;
mod thread;
mod time;
mod ui;

use std::io::Write;
use std::path::{Path, PathBuf};

use anyos_backend::{ExecutionBackend, GuestArch, GuestFault};
use anyos_loader::{LoadError, LoadOptions, Process};
use anyos_memory::MemoryError;
use anyos_pe::Machine;

pub use anyos_backend;

/// Where the guest's console output goes.
pub struct Stdio<'a> {
    pub stdout: &'a mut dyn Write,
    pub stderr: &'a mut dyn Write,
}

#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    /// Arguments after the program name, as the guest's command line.
    pub args: Vec<String>,
    /// Extra directories searched for application DLLs (the program's own
    /// directory is always searched first).
    pub search_dirs: Vec<PathBuf>,
    /// Print every API call and its result to the guest's stderr sink.
    pub trace: bool,
    /// Stop after this many guest instructions.
    pub instruction_limit: Option<u64>,
    /// Host folder the guest sees as `C:\` (an AnyOS environment's drive).
    /// Without one, only host paths (under `Z:\`) are reachable on Unix hosts.
    pub drive_c: Option<PathBuf>,
    /// Registry file (an environment's `registry.toml`) the guest reads and
    /// writes; changes are saved back when the program ends. Without one the
    /// registry lives only as long as the run.
    pub registry_file: Option<PathBuf>,
    /// Windows user name the guest sees (default: the host user name).
    pub user: Option<String>,
    /// Extra or overriding environment variables for the guest.
    pub env: Vec<(String, String)>,
    /// Command that runs another Windows program, for `CreateProcess`; the
    /// program's host path and its arguments are appended. Empty: child
    /// processes are not supported.
    pub child_runner: Vec<std::ffi::OsString>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutcome {
    pub exit_code: u32,
    pub instructions: u64,
    pub api_calls: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error(transparent)]
    Load(#[from] LoadError),
    #[error("guest memory: {0}")]
    Memory(#[from] MemoryError),
    #[error("{machine} code needs a backend this build does not have yet (the interpreter runs x86 and x64)")]
    UnsupportedMachine { machine: String },
    #[error("{0} is a DLL, not a program")]
    NotAnExecutable(String),
    #[error("{dll}!{symbol} is not implemented yet (called from {caller})")]
    UnimplementedApi {
        dll: String,
        symbol: String,
        caller: String,
    },
    #[error("the program crashed in {location}: {fault}")]
    Fault { fault: GuestFault, location: String },
    #[error("the program hit a breakpoint in {location}")]
    Breakpoint { location: String },
    #[error("{module} failed to initialise (its entry point returned FALSE)")]
    DllInitFailed { module: String },
    #[error("stopped after the {0}-instruction limit")]
    InstructionLimit(u64),
    #[error("the program can never continue: {detail}")]
    Deadlock { detail: String },
    #[error("the program raised exception {code:#010x} in {location} and has no handler AnyOS can run yet")]
    GuestException { code: u32, location: String },
}

/// Whether AnyOS implements `name` exported by `dll` (e.g. `kernel32.dll`),
/// for reports of which imports a program needs that are still missing.
pub fn implements(dll: &str, name: &str) -> bool {
    api::implemented(dll, name)
}

/// Load the Windows program at `path` and run it to completion on the
/// portable interpreter.
pub fn run_executable(
    path: &Path,
    opts: &RunOptions,
    stdio: Stdio<'_>,
) -> Result<RunOutcome, RunError> {
    let mut search_dirs = Vec::new();
    if let Some(dir) = path.parent() {
        search_dirs.push(if dir.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            dir.to_path_buf()
        });
    }
    search_dirs.extend(opts.search_dirs.iter().cloned());
    let process = Process::load_executable(
        path,
        &LoadOptions {
            search_dirs,
            load_dependencies: true,
            ..LoadOptions::default()
        },
    )?;
    let main = &process.modules[0];
    if main.image.is_dll {
        return Err(RunError::NotAnExecutable(main.name.clone()));
    }
    let program = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| main.name.clone());
    let mut backend = anyos_interp::Interpreter::new();
    run_process(process, &program, opts, &mut backend, stdio)
}

/// Run an already-loaded process on `backend`.
pub fn run_process(
    process: Process,
    program: &str,
    opts: &RunOptions,
    backend: &mut dyn ExecutionBackend,
    stdio: Stdio<'_>,
) -> Result<RunOutcome, RunError> {
    let arch = match process.machine {
        Machine::X64 => GuestArch::X86_64,
        Machine::X86 => GuestArch::X86,
        Machine::Arm64 | Machine::Arm64Ec => GuestArch::Arm64,
        other => {
            return Err(RunError::UnsupportedMachine {
                machine: other.display_name().to_string(),
            })
        }
    };
    if !backend.supports(arch) {
        return Err(RunError::UnsupportedMachine {
            machine: process.machine.display_name().to_string(),
        });
    }
    let mut cmdline = quote_arg(program);
    for a in &opts.args {
        cmdline.push(' ');
        cmdline.push_str(&quote_arg(a));
    }
    let mut t = thread::Thread::new(process, &cmdline, opts, backend, stdio)?;
    let exit_code = t.start()?;
    Ok(RunOutcome {
        exit_code,
        instructions: t.backend.instructions_executed(),
        api_calls: t.api_calls,
    })
}

/// Quote one argument the way `CommandLineToArgvW` expects.
fn quote_arg(a: &str) -> String {
    if !a.is_empty() && !a.contains([' ', '\t', '"']) {
        return a.to_string();
    }
    let mut out = String::from("\"");
    let mut slashes = 0;
    for ch in a.chars() {
        match ch {
            '\\' => slashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', slashes));
                out.push(ch);
                slashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', slashes * 2));
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::quote_arg;

    #[test]
    fn arguments_are_quoted_like_windows() {
        assert_eq!(quote_arg("plain"), "plain");
        assert_eq!(quote_arg("two words"), "\"two words\"");
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(quote_arg("dir\\ x\\"), "\"dir\\ x\\\\\"");
    }
}
