# Contributing

## Prerequisites

- Rust 1.82 or newer (stable) with `rustfmt` and `clippy` enabled.
- On macOS: Xcode Command Line Tools or the full Xcode.app for SDK headers,
  `ld`, `clang`, `llvm-lipo` and `llvm-dlltool`. The SDK toolchain from
  `xcrun` provides all of these; it is enough to build this workspace.
- On Windows and Linux: `clang`, `lld-link` and `llvm-dlltool` for the
  Windows/ELF fixtures (see `tests/fixtures/src/build.sh`).

## Formatting and linting

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

## Testing

Run the unit and integration tests in all crates plus the CLI tests:

```sh
cargo test
```

The fixture corpus (ignored tests) loads every `.exe`/`.dll` under a directory
twice and checks that every relocation lands at the same image offset:

```sh
ANYOS_CORPUS=/some/windows/bins cargo test -p anyos-loader -- --ignored --nocapture
```

## Cross-target builds

Build macOS targets on macOS with stable Rust:

```sh
rustup target add aarch64-apple-darwin x86_64-apple-darwin
cargo build --release --target aarch64-apple-darwin
cargo build --release --target x86_64-apple-darwin
```

The fixture build scripts (`build-other.sh`, `build.sh`) use the host SDK toolchain
from `xcrun` for the macOS slices and zig cc for the Linux slices.

## Commit style

Releases are cut from `main` with the [`release`](https://github.com/release-bank/release)
toolchain (conventional commits + `release drafter`). Keep the
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) format in
`CHANGELOG.md` and add a `RELEASE_NOTES_$VERSION.md` for every tagged release.
