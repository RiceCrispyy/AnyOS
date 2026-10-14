# Changelog

All notable changes to **AnyOS** will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [0.1.0] — Unreleased

**Added**

- Self-update (`anyos-update`). `anyos update` installs the newest GitHub release
  for this platform after checking it against `SHA256SUMS`; `--check` only
  reports. The desktop app checks on start and offers **Update and restart**,
  plus a **Check for updates** button. The running copy is swapped in place:
  `anyos` and `anyos-gui` beside it, the whole `AnyOS.app` on macOS, and on
  Windows the running `.exe` is renamed to `.old` and removed on the next launch.
  `--no-update-check` or `ANYOS_NO_UPDATE_CHECK=1` turns automatic checks off.
  Release builds now report the tag's version (`anyos --version`).
- `anyos-gui` — a desktop app for macOS, Windows and Linux (egui). Drop or choose
  a program (Windows `.exe`, Linux ELF, macOS Mach-O or `.app`, installers) to see
  its format, target OS and CPU and how AnyOS would run it, then run it and watch
  its output, with Stop and an option to log Windows API calls. Release archives
  include it, as `AnyOS.app` on macOS.
- `anyos run app.exe` — run Windows x86 and x64 console programs on a portable
  interpreter (`anyos-interp`) on any host, including Apple Silicon Macs. Windows
  API calls go through the call gate to Rust implementations (`anyos-win32`);
  the first are `ExitProcess`, `GetStdHandle`, `WriteFile`, `WriteConsoleA/W`,
  `GetLastError`/`SetLastError`, `GetCommandLineA/W`, `GetModuleHandleA/W` and
  the current process/thread queries. DLL entry points and TLS callbacks run
  before the program. `--trace` logs every API call. Unsupported APIs and
  instructions stop the run with a message naming them.
- `anyos-backend` — the `ExecutionBackend` trait shared by all execution backends.
- Release workflow: pushing a `v*` tag builds `anyos` for Linux x86-64, Windows
  x86-64 and macOS (universal) and attaches the archives and `SHA256SUMS` to the
  GitHub release.

- Standalone Rust runtime that can inspect and load Windows PE programs into an
  emulated guest process: map, relocate, bind imports, set up TLS, page protections,
  and an API call gate. ([README](README.md) "Quick start" and "Layout".)
- `anyos inspect` — detect and summarize Windows PE, Linux ELF, and macOS Mach-O
  programs without executing them; `--all` adds sections/imports/exports; `--json`
  emits a machine-readable report; `--host <target>` plans execution against another
  host.
- `anyos load` — dry-run assembly of a guest process (map, relocate, bind imports,
  TLS, protect). Works on Windows PE fixtures.
- `anyos redirect` — per-program network redirects in `<program>.anyos.toml` beside
  the program, with domain rules that cover subdomains. `add` / `list` / `test` /
  `remove`.
- `anyos run` — execute a Linux x86-64 **static** ELF on a Windows host via
  Vectored Exception Handling (VEH): patch `syscall`/`syscall` pairs to INT3, dispatch
  Linux x86-64 syscalls in-process, apply the guest FS base via `wrfsbase`, and map
  the stack correctly. Verified on the bundled static hello fixture
  (`tests/fixtures/bin/other/linux-x64-static`).

**Changed**

- Syscall coverage (Linux x86-64): added `open`/`openat`, `close`, `lseek`,
  `stat`/`lstat`/`newfstatat`, `ioctl`, `access`, `fcntl`, `getcwd`, `readlink`,
  `arch_prctl` (including `ARCH_SET_FS` and `ARCH_GET_FS`), `getdents64`,
  `clock_gettime`, `set_tid_address`, and `getuid`/`getgid`. Extended `read`/`write`/
  `writev` for guest file descriptors. ([crates/anyos-linux](crates/anyos-linux).)
- Windows VEH SYSCALL-trap handler: no longer rejects the patched INT3 trap when
  Rip points at the `0xCC` itself (previously every syscall was killed with
  0x80000003 / exit code 3). ([crates/anyos-exec](crates/anyos-exec).)

**Known limitations**

- This version **cannot execute guest code yet** as a general capability. README.md
  says so explicitly. Read that sentence before you read anything into the scope of a
  release: the project is milestone-based and milestone 3 (guest execution) is in
  progress.
- `anyos run` currently runs only **static** Linux x86-64 ELF on a Windows host.
  Dynamically-linked Linux ELF needs the PT_INTERP interpreter (`/lib64/ld-linux-x86-64.so.2`
  on Linux) which is not present on a Windows host; that path is not implemented yet.
- `anyos run` for macOS (Mach-O) programs is not yet implemented. Mach-O is detected
  and summarized by inspection, but execution is not available.
- The project is source-available under the **PolyForm Noncommercial License 1.0.0**.
  Commercial use is not permitted under that license; see [COMMERCIAL.md](COMMERCIAL.md).
- The runtime runs Linux x86-64 code on an x86-64 host only. ARM64 Linux ELF is
  rejected (HostMismatch), and cross-arch execution is not supported.
- Linux syscall coverage is still incomplete relative to what real programs exercise
  early in startup; unimplemented syscalls return `ENOSYS`. Coverage grows as the
  corpus test ([README "Tests"](#tests)) reveals needs.
- Guest memory is emulated (an AddressSpace with page protections), not backed by the
  host process memory in a way that survives arbitrary guest writes; the shadow/sync
  design is still narrow and is optimized around the current tests.

## Testing

```sh
cargo test                       # 36 tests: fixtures, malformed input, mutation fuzzing, CLI
ANYOS_CORPUS=/some/windows/bins cargo test -p anyos-loader -- --ignored --nocapture
```

Fixtures are rebuilt with `bash tests/fixtures/src/build.sh` (needs clang, lld-link,
llvm-dlltool; no Windows SDK). The output is byte-for-byte reproducible.
