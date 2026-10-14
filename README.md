# AnyOS

A standalone runtime written in Rust that aims to run programs built for one OS
on another, starting with Windows programs on macOS (then Linux and ChromeOS). It
does not use Wine and does not need a VM.

**Status:** milestones 1 and 2 are done. AnyOS can inspect Windows PE files and
load them into an emulated guest process: map, relocate, bind imports and set up
TLS. It also recognises Linux (ELF) and macOS (Mach-O) programs and says how it
would run them, and it keeps per-program network redirects. It cannot execute
guest code yet. See [docs/architecture.md](docs/architecture.md).

## Quick start

```sh
cargo build --release
./target/release/anyos inspect path/to/app.exe          # what is it, what would it need
./target/release/anyos inspect --all path/to/app.exe    # + sections, imports, exports
./target/release/anyos inspect --json path/to/app.exe   # machine-readable, versioned report
./target/release/anyos load path/to/app.exe --imports   # dry-run load into a guest process
./target/release/anyos load --rebase path/to/app.exe    # force relocation
./target/release/anyos inspect --host macos-arm64 app.exe  # plan for another host
./target/release/anyos inspect some-linux-program          # ELF and Mach-O work too
./target/release/anyos inspect setup.exe                   # installers: NSIS, Inno, MSI, .deb, .pkg, ...
```

### Per-program network redirects

A program's own traffic can be sent to a different server without touching the
host's hosts file or DNS. The setting lives in `<program>.anyos.toml` beside the
program and applies only when AnyOS runs that program. A domain rule also covers
its subdomains.

```sh
anyos redirect add  Game.exe oldgame.example 203.0.113.10   # writes Game.anyos.toml
anyos redirect test Game.exe game.oldgame.example             # -> 203.0.113.10:80
anyos redirect list Game.exe
anyos redirect remove Game.exe oldgame.example
```

Example (abridged; `uses_dll.exe` imports from `exports_dll.dll` sitting beside it):

```
$ anyos load --rebase --imports tests/fixtures/bin/x86/uses_dll.exe
Module uses_dll.exe
  Base:        0x10000 (rebased from 0x400000)  size 0x5000
  Relocations: 5 applied
  Imports:     3 bound (2 to guest modules, 1 to the API call gate)
    exports_dll.dll!anyos_add  IAT 0x1206c -> 0x10001000  (export of exports_dll.dll)
    exports_dll.dll!#7  IAT 0x12070 -> 0x10001020  (export of exports_dll.dll)
    kernel32.dll!ExitProcess  IAT 0x12078 -> 0x20000  (call gate stub #0)
```

First host: macOS. It builds with stable Rust on Apple Silicon and Intel Macs
(`brew install rust`, or rustup). The macOS plan is in the architecture doc.

## Layout

```
crates/anyos-pe       PE/COFF inspection, DLL catalog, compatibility report (never executes)
crates/anyos-memory   guest address space: regions, page protections, checked access
crates/anyos-loader   process assembly: map, relocate, load DLLs, bind, TLS, protect; API call gate
crates/anyos-format   format detection (PE, ELF, Mach-O incl. universal) and any-OS run plan
crates/anyos-net      per-program network policy (redirects) and the <program>.anyos.toml profile
crates/anyos-package  installer and package identification (NSIS, Inno, MSI, .deb, .pkg, ...)
apps/anyos-cli        the `anyos` command
tests/fixtures        tiny test programs: Windows (x64, ARM64, x86), Linux and macOS + build scripts
docs/architecture.md  design, interfaces, security model, roadmap
```

## Tests

```sh
cargo test                       # 36 tests: fixtures, malformed input, mutation fuzzing, CLI
ANYOS_CORPUS=/some/windows/bins cargo test -p anyos-loader -- --ignored --nocapture
```

The corpus test loads every `.exe` and `.dll` under a directory twice, once at
the preferred base and once at a forced base, and checks that every relocation
lands at the same image offset. It has been run against ripgrep (x64), uv/uvx
(ARM64), WebView2Loader (x64, ARM64, x86), MSVC app-runtime DLLs and about 40
.NET assemblies (67 images in total). It is not part of `cargo test`.

Fixtures are rebuilt with `bash tests/fixtures/src/build.sh`. This needs clang,
lld-link and llvm-dlltool, but no Windows SDK. The output is byte-for-byte
reproducible.

## Principles

- Inspection is separate from execution. Parsing is bounded, and malformed files
  fail cleanly.
- The loader never implements Windows APIs. System imports are bound to call-gate
  stubs, so guest code reaches the runtime through one narrow, auditable door.
- Host-specific code stays out of the core. Third-party code (goblin for PE
  headers) is wrapped behind AnyOS's own types.

## License

AnyOS is source-available under the
[PolyForm Noncommercial License 1.0.0](LICENSE.md). Individuals, hobbyists,
students, researchers, charities, schools and government bodies can use, change
and share it for free, as long as they keep the copyright notice and license
with it. Businesses and other commercial users need a separate license; see
[COMMERCIAL.md](COMMERCIAL.md). Third-party files in `tests/fixtures/third_party`
keep their own licenses.
