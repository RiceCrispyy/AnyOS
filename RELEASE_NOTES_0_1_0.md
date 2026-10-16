# AnyOS 0.1.0

AnyOS is a standalone Rust runtime that aims to run programs built for one OS on
another, without Wine and without a VM.

**Status of this release.** Milestones 1 and 2 are done. This release can **inspect**
Windows PE, Linux ELF, and macOS Mach-O programs, **load** a Windows PE program into an
emulated guest process (dry run), and manage per-program network redirects. It can also
**run a Linux x86-64 static ELF on a Windows host** via Vectored Exception Handling, which
is the first step of the execution milestone.

Do **not** read more into this release than the bullets below. README.md says the same
thing: the project still **cannot execute guest code yet** as a general capability.

## What works in 0.1.0

- `anyos inspect <program>` — detect and summarize PE, ELF, or Mach-O programs, without
  executing them. With `--all`, also show sections, imports, and exports. With `--json`,
  emit a machine-readable report. With `--host <target>`, plan execution against another
  host.
- `anyos load --imports <pe.exe>` — dry-run assembly of a guest process: map, relocate,
  bind imports, TLS, page protections.
- `anyos load --rebase <pe.exe>` — dry-run with forced relocation.
- `anyos redirect add` / `list` / `test` / `remove` — per-program network redirects in
  `<program>.anyos.toml` beside the program, including subdomain rules.
- `anyos run <linux-x86-64-static>` — run a **static** Linux x86-64 ELF on a **Windows**
  host, printing guest output and returning the guest exit code. Verified on the bundled
  static hello fixture (`tests/fixtures/bin/other/linux-x64-static`), which prints
  `hello from anyos fixture` and exits 0.

## What does **not** work yet

- **macOS execution.** Mach-O is detected and summarized, but is not executed. The project
  does not yet run macOS programs.
- **Dynamic Linux ELF on Windows.** Dynamically-linked Linux x86-64 ELF needs the PT_INTERP
  interpreter (`/lib64/ld-linux-x86-64.so.2` on Linux) which is not present on a Windows host.
  That execution path is not implemented yet.
- Cross-arch execution. Linux x86-64 is run only on an x86-64 host. ARM64 Linux ELF is
  rejected (`HostMismatch`).
- The broader execution milestone. Linux syscall coverage is still incomplete relative to
  what real programs exercise early in startup; unimplemented syscalls return `ENOSYS`.
- Guest memory is emulated (an AddressSpace with page protections), not backed by the host
  process in a way that survives arbitrary guest writes yet. The shadow/sync design is still
  narrow and tuned to the current tests.

## Installation

```sh
cargo build --release
./target/release/anyos --version     # reports 0.1.0
```

Builds with stable Rust.

## License

AnyOS is source-available under the [PolyForm Noncommercial License 1.0.0](LICENSE.md).
Individuals, hobbyists, students, researchers, charities, schools, and government bodies can
use, change, and share it for free as long as they keep the copyright notice and license with
it. Businesses and other commercial users need a separate license; see [COMMERCIAL.md](COMMERCIAL.md).
