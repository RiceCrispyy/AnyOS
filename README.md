# AnyOS

A standalone runtime written in Rust that aims to run programs built for one OS
on another, starting with Windows programs on macOS (then Linux and ChromeOS). It
does not use Wine and does not need a VM.

**Status:** milestones 1 and 2 are done and milestone 3 has started. AnyOS can
inspect Windows PE files and load them into an emulated guest process: map,
relocate, bind imports and set up TLS. Small Windows console programs (x86 and
x64) now run end to end on a portable interpreter on any host, including Apple
Silicon Macs, with Windows API calls served through the call gate. Most of
`kernel32` is there: threads and synchronisation, heaps and virtual memory,
files, the environment, time, code pages, locale and resources, plus the
`advapi32` registry (kept in the environment's `registry.toml`) and user32's
string helpers. GUI APIs come later. AnyOS also recognises Linux (ELF)
and macOS (Mach-O) programs and says how it would run them, and it keeps
per-program network redirects. See [docs/architecture.md](docs/architecture.md).

## Download

Prebuilt `anyos` binaries for Linux (x86-64), Windows (x86-64) and macOS
(universal, Apple Silicon and Intel) are attached to each
[GitHub release](https://github.com/RiceCrispyy/AnyOS/releases). Unpack the
archive and run `anyos` from a terminal, or open the desktop app (`AnyOS.app` on
macOS, `anyos-gui` elsewhere). The binaries are not signed yet, so Windows
SmartScreen and macOS Gatekeeper may ask you to confirm the first launch.

### Updating

Release builds keep themselves up to date. The desktop app checks GitHub when it
starts and offers **Update and restart** when a newer release is out (or press
**Check for updates**). From a terminal:

```sh
anyos update           # download, verify and install the newest release
anyos update --check   # only say whether there is one
```

The download is checked against the release's `SHA256SUMS` before anything is
replaced. The update swaps in `anyos`, the app beside it, and on macOS the whole
`AnyOS.app`. The CLI also looks once a day, after a command, and mentions a new
version on stderr. Turn the automatic checks off with `--no-update-check` (CLI
and app) or `ANYOS_NO_UPDATE_CHECK=1`. Builds you make yourself never check on
their own. Releases up to v0.1.13 predate the updater, so install one newer
release by hand first.

## Desktop app

`anyos-gui` is the point-and-click way in. Drop a program onto the window (or
choose one), and it shows what the program is, which OS and CPU it was built for,
and how AnyOS would run it on this computer. Then press **Run** and watch its
output, or **Stop** it. Installers get an **Install** button instead, and programs
built for this computer start directly. It works on macOS, Windows and Linux, and
runs programs through the `anyos` tool that ships beside it, so a crashing program
cannot take the window down.

```sh
cargo run --release -p anyos-gui                 # empty window: drop a program in
cargo run --release -p anyos-gui -- app.exe      # open a program straight away
```

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
./target/release/anyos run tests/fixtures/bin/x64/hello_console.exe   # run a Windows console program
./target/release/anyos run --trace app.exe                 # + log every Windows API call
./target/release/anyos run --env games app.exe             # see the C: drive and registry of an environment
```

A Windows program sees an environment's `C:` drive and registry: the one it is
installed in, the one `--env` names, or the default environment when it
exists. Host folders appear under `Z:`. `CreateProcess` starts the child as
another `anyos run` in the same environment.

`anyos run` stops with a clear message naming the first Windows API or
instruction it does not support yet (for example `kernel32.dll!HeapAlloc is not
implemented yet`), so the gaps show up one at a time.

### Installing apps

`anyos install` puts an app into an AnyOS environment (its own `C:` drive,
registry and app list) instead of onto your computer. It unpacks the installer
itself, so no installer code runs. Supported now: `.msi`, ZIP and 7-Zip archives,
and ZIP/7-Zip self-extracting `.exe` files. Inno Setup and NSIS are next.

```sh
./target/release/anyos install app.msi             # into the "default" environment
./target/release/anyos install game.zip --env games
./target/release/anyos apps                        # what is installed, and where
./target/release/anyos uninstall app
./target/release/anyos env default                 # where an environment lives
```

Installed programs cannot be started yet; that comes with the execution work.

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
crates/anyos-env      environments: private C: drive, registry, installed-app index
crates/anyos-install  installs MSI, ZIP and 7-Zip packages into an environment
crates/anyos-update   self-update: latest GitHub release, SHA-256 check, in-place swap
apps/anyos-cli        the `anyos` command
apps/anyos-gui        desktop app: pick a program, see its format and run plan, run it
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
