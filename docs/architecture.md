# AnyOS architecture

AnyOS is a standalone runtime, written in Rust, that runs Windows PE programs on
Linux, macOS and ChromeOS, with the long-term aim of running any OS's executables on any OS. It reads and loads Windows executables itself, provides
the Windows APIs they call by translating them to host services, and runs their
code natively when the CPU matches or through binary translation when it does not.
It does not use Wine code and is not a Wine clone. Its structure follows the needs
of a Rust runtime that the project owns, not Wine's DLL-for-DLL layout.

This document describes the design and where the code stands.

## Goals and non-goals

Goals, in priority order:

1. **Correctness you can measure.** Every capability has fixtures and tests. The
   compatibility report never claims more than the code can do.
2. **Security.** Executables are untrusted input. Inspection is kept separate from
   execution, parsing is bounded, and nothing is called "sandboxed" until a host
   enforces the isolation.
3. **Performance.** Native execution when guest and host CPUs match. Translation
   only when they don't.
4. **Portability.** One shared core. Host-specific code lives only in host adapters.
5. **Scalability.** Windows API providers, execution backends and host adapters
   plug in behind narrow interfaces, so coverage can grow without touching the core.

Non-goals for now: kernel-mode drivers, anti-cheat, and promising universal
compatibility. Progress is measured by a published compatibility matrix.

## Toward any OS

The long-term goal is wider than Windows: run executables built for any OS on
any OS. Windows PE on macOS comes first. The layers below are split so that
other guests can be added beside it without changing the core:

| Layer | Windows guest (now) | Later guests |
|---|---|---|
| Format inspector | `anyos-pe` | `anyos-format`: ELF (Linux, Android, FreeBSD) and Mach-O (macOS, universal) summaries, built |
| Loader | PE mapping, relocations, imports | ELF dynamic loader, dyld-style binding |
| OS personality | Win32/NT APIs behind the call gate | Linux syscalls, Darwin syscalls and frameworks |
| Execution backends | shared | shared (native, interpreter, translator) |
| Guest memory, host services | shared | shared |

The execution backends, guest memory and host adapters are format-neutral, and
every personality reaches the runtime the same way: through a trap into one
narrow gate.

`anyos-format::detect` is the front door: it sends PE files to `anyos-pe` and
summarises ELF and Mach-O files (CPU per slice, linked libraries, dynamic
loader, minimum OS). `anyos-format::plan` then picks the slice matching the
host CPU and one of four modes: *host native* (the host can already run it, so
AnyOS steps aside; this includes x86-64 Mac programs under Rosetta), *native
CPU* (same CPU, AnyOS supplies the OS personality), *translated* (CPU
translation plus personality), or *unsupported*. Loaders and personalities for
ELF and Mach-O guests are not built yet; PE-only code stays in the PE crates.

### Per-program network policy (`anyos-net`)

Network redirects belong to the runtime, not the host. Each program can have a
profile, `<program>.anyos.toml` beside it, with a `[network.redirects]` table
mapping a domain or IP to a host, IP or host:port. A domain rule covers its
subdomains and the most specific rule wins. When the networking personality
exists (Winsock `getaddrinfo`/`gethostbyname`/`connect`, and their Linux and
Darwin equivalents), every name lookup and connect from the guest goes through
`NetPolicy::rewrite`/`resolve`, so redirects affect only that program and need
no administrator rights, hosts-file edits or DNS changes. `anyos redirect`
edits and tests profiles today.

## Layers

```
            anyos CLI / app manager  (inspect, load, run, doctor, prefixes)
                              |
                 Executable classification  ......... anyos-pe        [built]
        PE format, CPU, subsystem, imports, .NET, readiness report
                              |
             +----------------+-----------------+
             |                                  |
      Native PE loader  ....... anyos-loader   Managed runtime adapter  [planned]
      [built: map, relocate,                   (CoreCLR via hostfxr)
       bind, protect, TLS]                      |
             |                                  |
             +----------------+-----------------+
                              |
             Windows API providers  (kernel32, ntdll, user32, gdi32, ...)  [planned]
               reached only through the API call gate  ... CallGate [built]
                              |
          +-------------------+--------------------+
          |                                        |
   Execution backends  [planned]            Host services  [planned]
   native / interpreter / translator        files, windows, audio, input,
                                            network, graphics
          |                                        |
          +-------- guest memory ... anyos-memory [built: emulated backend]
                              |
                 Linux / macOS / ChromeOS host adapters  [planned]
```

## Crates

| Crate | Status | Responsibility |
|---|---|---|
| `crates/anyos-pe` | built | Bounded PE/COFF parsing into an owned `PeImage`. DLL family catalog. Versioned compatibility report. Never executes anything. |
| `crates/anyos-memory` | built (emulated backend) | Guest address space: 64 KiB-granular placement, 4 KiB page protections, checked guest access and privileged loader access. |
| `crates/anyos-loader` | built | `Process`: maps images, applies base relocations, loads application DLLs, binds imports to guest exports or call-gate stubs, sets TLS slots, applies page protections. |
| `crates/anyos-format` | built | Format detection (PE, ELF, Mach-O, universal) and the any-OS run plan. |
| `crates/anyos-net` | built | Per-program network policy (redirects) and the `<program>.anyos.toml` profile. |
| `crates/anyos-package` | built (identification) | Recognises installers and packages (NSIS, Inno Setup, MSI, WiX Burn, `.deb`, `.rpm`, AppImage, `.pkg`, `.dmg`, ZIP, 7-Zip) and picks the install route. |
| `crates/anyos-env` | built | Environments: private `C:` drive, path mapping, registry store, installed-app index. |
| `crates/anyos-install` | built (MSI, ZIP, 7-Zip) | Unpacks installers into an environment without running them; uninstall. |
| `apps/anyos-cli` | built | `anyos inspect` (PE, ELF, Mach-O), `anyos load` (dry run), `anyos redirect`. |
| `crates/anyos-backend` | built | `ExecutionBackend` trait, guest CPU context (`GuestContext`, `X86Context`), `StopReason` and `GuestFault`. |
| `crates/anyos-interp` | built (integer subset) | Portable x86/x64 interpreter backend: general-purpose integer instructions, string ops, `fs`/`gs` bases, SSE moves and SSE2 packed integers. No x87 or SSE floating point yet. |
| `crates/anyos-win32` | built (first APIs) | Windows personality: stack, TEB/PEB, static TLS, DLL and TLS initialisation, call-gate dispatch with x64 and stdcall conventions, first `kernel32` APIs. |
| `crates/anyos-exec` | built (Linux ELF) | Runs Linux x86-64 ELF programs (native on Windows via syscall trapping, directly on Linux). |
| native and translating backends | later | Direct execution on a matching CPU, and a translator. |
| `anyos-win32-abi` + `anyos-api-*` | later | Calling conventions, struct layouts, error codes, and API providers grouped by family. |
| `anyos-host` + `anyos-host-{linux,macos,chromeos}` | later | Host services behind one interface. |
| `anyos-managed` | later | .NET hosting through hostfxr/CoreCLR. |

Dependency rules:

- `anyos-pe` depends on nothing in AnyOS and never executes code.
- The loader never implements Windows APIs. It hands imports to the call gate.
- API providers reach guest memory only through `anyos-memory` and reach the host
  only through host-service traits. Raw host pointers are never shared across
  module boundaries.
- Host adapters contain no app-specific hacks. Translators and the managed runtime
  sit behind features or capability discovery.

## Pipeline

### 1. Inspect (`anyos-pe`)

goblin parses the DOS/NT headers and the section table. AnyOS parses the data
directories itself, through a bounds-checked RVA reader (`view.rs`): imports,
delay imports, exports (including forwarders and NONAME ordinals), base
relocations, TLS (with callbacks), and the CLR header plus metadata version. This
keeps the semantics the loader depends on, such as IAT slot addresses, ordinal
flags and relocation types, explicit and tested in one place.

Robustness rules:

- Every count is capped by `Limits`: sections (96, the Windows limit), imports,
  exports, relocations, TLS callbacks, image size and file size.
- A damaged *optional* directory doesn't stop inspection. It is listed in
  `broken_directories` and the rest of the image is still described. The loader
  refuses any image that has one.
- Section layout is validated: sections must stay inside `SizeOfImage`, must not
  overlap, and alignments must be powers of two.

The **compatibility report** (`report.rs`, `REPORT_VERSION = 1`) sorts each import
into an API family (`catalog.rs`). This covers API-set contracts such as
`api-ms-win-core-*` and versioned CRT names such as `vcruntime140_1`. Windows
system DLLs that AnyOS must provide are told apart from application DLLs that
must sit next to the program. The report picks an execution mode for the current
host, lists the capabilities the program needs with honest status values
(implemented / planned / unsupported), and gives a readiness verdict. Later
milestones will use this report to choose a backend and API providers before
launch.

### 2. Load (`anyos-loader`)

`Process::load_executable` works like the Windows loader, in phases:

1. **Map** each image at its preferred base when that range is free. Otherwise it
   is mapped at a new 64 KiB-aligned base, unless relocations are stripped.
   Headers and the file-backed part of each section are copied. The rest stays
   zero.
2. **Relocate**: `DIR64`, `HIGHLOW`, `HIGH`, `LOW` and `HIGHADJ`, using the same
   arithmetic as `LdrProcessRelocationBlock`. Unknown types are refused.
3. **Load dependencies**: application DLLs found in the search directories are
   mapped breadth-first. Mapping all modules before binding any of them is what
   makes circular imports work.
4. **Bind** every IAT slot to an export of a loaded guest module (by name or
   ordinal, following forwarders with a depth limit). If no guest module provides
   the export, the slot gets a **call-gate stub**. Delay-load slots are left alone,
   as Windows does.
5. **TLS**: each module with TLS gets a process-wide slot index, written to
   `AddressOfIndex`. Its callbacks are read back from relocated memory.
6. **Protect**: headers become `r--`, and each section gets the access its
   characteristics ask for. Images with low section alignment become one `rwx`
   view, as on Windows.

If an image fails partway through, its mapping is removed again.

### The API call gate

Imports that AnyOS must implement are never bound to host function pointers.
Each distinct `(dll, symbol)` gets a 16-byte stub inside a dedicated
execute-only region. The stub is filled with the architecture's trap
instruction (`INT3` on x86, `BRK #0xF000` on ARM64). When the execution
backend stops at a stub address, it calls `CallGate::lookup`, which returns
the DLL and symbol. The backend then hands the guest CPU context to the Rust
implementation of that API. This gives one narrow, auditable boundary between
guest code and the runtime that works the same way for native execution,
interpretation and translation. It also lets the runtime report unimplemented
APIs by name at the moment they are first called.

### 3. Execute (in progress)

The interface (`crates/anyos-backend`):

```rust
pub trait ExecutionBackend {
    fn supports(&self, arch: GuestArch) -> bool;
    /// Run a guest thread until it traps, faults, or uses up `limit` instructions.
    fn run(&mut self, ctx: &mut GuestContext, mem: &mut AddressSpace, limit: Option<u64>) -> StopReason;
}
pub enum StopReason { Breakpoint { address: u64 }, Fault(GuestFault), InstructionLimit }
```

Backends know nothing about Windows. A call-gate stub is a trap instruction, so
reaching one is a `Breakpoint` inside the gate region. The Windows personality
(`crates/anyos-win32`) looks the address up in the `CallGate`, reads the
arguments with the right convention (Microsoft x64, or stdcall on x86), runs the
Rust implementation, writes the result and returns to the guest's caller. The
runtime calls into guest code (DLL entry points, TLS callbacks, the program's
entry point) by pushing the address of a private gate stub as the return
address, so a `Breakpoint` there means the call has returned. Memory is the
emulated `AddressSpace` for now; a native backend will need host-mapped guest
memory behind the same operations.

`anyos run app.exe` runs a Windows x86 or x64 program this way on the
interpreter. The interpreter is checked against a real CPU:
`tests/fixtures/src/selftest.h` is built both as a Windows program and natively,
and the two outputs must match exactly (at -O0 through -O3).

Order of work: (a) a reference **interpreter** for a small x86-64 subset, used to
run the fixtures end to end and test API semantics deterministically; (b) a
**native** backend for matching CPUs, which needs a host `mmap` implementation of
guest memory at the guest's own addresses and signal-based call-gate traps;
(c) **translation**, to run x86-64 on ARM64 (Apple Silicon, ARM Chromebooks).
For translation, QEMU TCG and FEX-Emu are being evaluated for licensing and
embeddability before anything is written from scratch.

## macOS first

macOS is the first host. These are the facts about macOS that shape the
design. Anything marked *verify* still
needs a prototype on real hardware.

| Topic | Apple Silicon (ARM64) | Intel Mac |
|---|---|---|
| x86-64 Windows apps (most software) | Translate. Fastest path: build AnyOS as an `x86_64-apple-darwin` binary so Rosetta 2 translates the guest code along with AnyOS, the way CrossOver/Wine work today. Long term: our own translator, because Apple said at WWDC 2025 that Rosetta 2 will be cut back after macOS 27 (*verify*). | Native |
| ARM64 Windows apps | Same instruction set, with two blockers. macOS reserves register `x18`, which Windows ARM64 code uses as its TEB pointer (*verify* whether the kernel clobbers it). Pages are 16 KiB, while Windows images are laid out for 4 KiB pages. | Translate |
| 32-bit x86 apps | Translate (no 32-bit processes on macOS) | 32-on-64 thunking or translation |
| .NET IL-only apps | Native .NET runtime (osx-arm64) | Native .NET runtime (osx-x64) |

Other macOS constraints:

- **16 KiB pages.** `anyos inspect --host macos-arm64` counts the host pages that
  would mix section protections. Our ARM64 fixtures and the real `uvx.exe` both
  have some (4 of 21 pages for `uvx.exe`). A native backend must merge those
  pages (the strictest safe union) or emulate them.
- **W^X.** Executable memory on Apple Silicon needs `MAP_JIT` and per-thread
  write/execute switching. Signed builds also need the JIT entitlements.
  Translated and loaded code both fall under these rules.
- **TEB access.** Windows x64 code reads its TEB through `gs`. macOS owns the
  `gs` base (it points at pthread thread-specific data), so the TEB layout has
  to coexist with it. This is the first thing the Rosetta spike must prove.
- **Host services later.** AppKit windows (via winit), Metal (via wgpu) for
  graphics, CoreAudio for sound. APFS is case-insensitive by default, which
  suits Windows path semantics.

### Revised milestone 3 for macOS

1. **Portable interpreter backend.** *Done.* `hello_console.exe` (x64 and x86)
   runs end to end: `GetStdHandle` and `WriteFile` go through the call gate to
   host stdout. This works on any Mac with no Rosetta or JIT entitlement, and it
   fixes the `ExecutionBackend` and call-gate dispatch interfaces. Still missing
   for real programs: x87 and SSE floating point, cdecl APIs (`msvcrt`), a heap,
   files, and structured exception handling.
2. **macOS guest memory backend.** Map guest images at their Windows addresses
   with `mmap` and handle 16 KiB pages.
3. **Rosetta spike (Apple Silicon).** Build AnyOS for x86-64, run the x64 fixture's
   code directly, catch call-gate traps with a `SIGTRAP` handler, and prove the
   TEB/`gs` layout.
4. **ARM64 spike.** Measure whether `x18` survives on macOS and decide whether
   ARM64 Windows code can run natively.

## Installing apps

Most apps ship as installers, not as a bare `.exe`. AnyOS installs them into its
own per-user **environment** rather than onto the host, so nothing lands in the
host's `Applications`, `/usr` or registry-like stores.

`anyos-package` recognises the package first (`anyos inspect` shows it):

| Package | OS | First route | Silent switches |
|---|---|---|---|
| NSIS `.exe` | Windows | extract | `/S` |
| Inno Setup `.exe` | Windows | extract | `/VERYSILENT /SUPPRESSMSGBOXES /NORESTART` |
| MSI / MSP | Windows | extract (AnyOS reads the MSI tables itself) | `/qn` |
| 7-Zip / ZIP self-extractor | Windows | extract | |
| WiX Burn bundle, InstallShield | Windows | run the installer | `/quiet`, `/s` |
| `.deb`, `.rpm`, AppImage | Linux | extract | |
| `.pkg`, `.dmg` | macOS | extract | |

Two routes, used in this order:

1. **Extract.** When the payload format is documented, AnyOS unpacks it in Rust
   and maps the installer's directories (`{app}`, `$INSTDIR`, `ProgramFilesFolder`)
   into the environment. For MSI this also applies the `Registry` and `Shortcut`
   tables. No guest code runs, so this works before the execution milestones.
   Custom actions and installer scripts are skipped and reported.
2. **Run.** Otherwise the installer runs as a guest with its silent switches, so it
   needs the console-level Windows API (milestone 4) plus the registry,
   shell folders, `CreateProcess` and shortcuts, but not the GUI layer.

### Environments (`anyos-env`)

```text
<data dir>/envs/<name>/
  env.toml                  name and the Windows user name apps see
  apps.toml                 installed apps: files, folders, registry values, shortcuts
  windows/registry.toml     registry keys and values
  windows/drive_c/          Program Files, Program Files (x86), ProgramData,
                            Users/<user>/AppData, Windows/System32, ...
```

The data directory is `$ANYOS_HOME`, else `~/Library/Application Support/AnyOS`
(macOS), `%LOCALAPPDATA%\AnyOS` (Windows) or `$XDG_DATA_HOME/anyos`. Windows
paths map to host paths through `Env::host_path`, which matches names
case-insensitively and refuses other drives and `..`, so no package can write
outside its environment. The same mapping will back the file APIs once programs
run. Linux and macOS packages will get matching trees (`usr/`, `Applications/`).

### What `anyos install` does today (`anyos-install`)

- **MSI:** resolves the `Directory` table (standard folders follow 64-bit
  Windows rules, so `ProgramFilesFolder` is `C:\Program Files (x86)`), writes
  every `File` row out of its cabinet (embedded or beside the `.msi`), creates
  `CreateFolder` folders, applies `Registry` with formatted strings expanded,
  records `Shortcut` rows, and adds an Add/Remove Programs key. Every feature and
  component is installed. Custom actions, conditions and tables such as
  `ServiceInstall` or `Environment` are skipped and listed in the app's notes.
- **ZIP / 7-Zip archives and self-extractors:** unpacked into
  `C:\Program Files\<name>`, where the name is the archive's single top folder
  or its file name.
- `anyos uninstall` removes exactly what was recorded: files no other app also
  installed, folders left empty, and the registry values and keys it created.
- The main program is the shortcut target when there is one, else the most
  likely `.exe` (not an uninstaller, updater or helper).

Checked against the real PowerShell 7.4.5 MSI: all 912 files match the
package's sizes and the 151 hashes it carries.

## Guest memory

`anyos-memory` currently stores guest memory in host heap buffers. This is
enough for the loader, the interpreter and tests. It already applies Windows
rules: the first 64 KiB is never mapped, the user-mode ceiling depends on
pointer size and the large-address-aware flag, 64 KiB allocation granularity,
4 KiB pages, a memory budget, and guest accesses that are checked against page
protections. A native backend will implement the same operations with host
`mmap`/`mprotect` at identical guest addresses. The loader's use of privileged
writes (`*_raw`) marks the places where it acts as the kernel.

## Security model

- Inspection never runs code, and all parsing is bounded. Mutation-fuzz tests
  cover both the parser and the loader.
- Untrusted images are handled as data until the execution layer exists. When it
  does, native execution will run in a separate worker process with a narrow IPC
  interface and host-enforced restrictions (seccomp/landlock on Linux, the
  sandbox profile on macOS). Until that work is done and tested, the docs will not
  call AnyOS a sandbox.
- No elevated execution, per-application writable prefixes, and audited loading
  of any native host libraries.

## Testing

- `tests/fixtures/src`: tiny C programs built with clang + lld-link for x64,
  ARM64 and x86 (`build.sh`, reproducible with `/Brepro`). They cover console and
  GUI subsystems, delay loading, a DLL with named, NONAME and data exports, an
  executable that imports by ordinal, base relocations, and a TLS directory with a
  callback.
- `tests/fixtures/third_party`: one small MIT-licensed IL-only .NET assembly.
- Unit and integration tests in each crate. Malformed-input tests cover every
  truncation, targeted header corruption, damaged directories and limits, and
  thousands of random mutations per fixture.
- `ANYOS_CORPUS=/dir cargo test -p anyos-loader -- --ignored` loads every
  `.exe`/`.dll` under a directory at both the preferred base and a forced base,
  and checks that every fixup lands at the same image offset. Real-world
  binaries are not committed.

## Roadmap

| Milestone | Status |
|---|---|
| 1. PE inspector (`anyos inspect`) | Done |
| 2. PE loader (map, relocate, bind, TLS, protections) | Done (emulated guest memory) |
| 3. Execution prototype on macOS: interpreter, call-gate dispatch, macOS guest memory, Rosetta and ARM64 spikes (see above) | Interpreter and call-gate dispatch done; macOS guest memory next |
| 4. Console compatibility: kernel32/ntdll/CRT subset, files, errors, threads | |
| 4a. Installing apps: environments, `anyos install` (extract route first, then silent run) | Identification, environments, MSI/ZIP/7-Zip done; Inno and NSIS next |
| 5. Basic Win32 GUI on macOS: windows, messages, input, GDI | |
| 6. Modern .NET via hostfxr/CoreCLR | |
| 7. Linux and ChromeOS adapters, own x86-64 to ARM64 translator | |
| 8. Hardening: isolation, D3D, COM, wider coverage | |

## Open decisions

- **License.** PolyForm Noncommercial 1.0.0, with separate commercial licenses on
  request (COMMERCIAL.md). Contributions need a contributor agreement so they can
  be offered under both. Dependencies must be permissive (MIT, Apache-2.0, BSD,
  zlib); copyleft code such as QEMU (GPL) cannot be linked in.
- **Translator.** QEMU TCG vs FEX-Emu (MIT) vs a purpose-built translator. QEMU's
  GPL rules it out as linked code under the chosen license; this also depends on
  whether macOS hosts are supported.
- **Native guest memory on macOS.** Mapping x86-64 guests at Windows addresses on
  Apple Silicon needs a translator anyway. This decides whether "native" means
  only same-CPU hosts.
