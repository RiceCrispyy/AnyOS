# Building for macOS

## What is needed

Building the macOS slices of the fixtures needs the host's macOS SDK
toolchain, not a separate toolchain installation:

- `clang` (from `xcrun --sdk macosx --find clang`)
- `ld` (from `xcrun --sdk macosx --find ld`)
- `llvm-lipo` (from `xcrun --sdk macosx --find llvm-lipo`)
- `llvm-dlltool` (from `xcrun --sdk macosx --find llvm-dlltool`)
- `llvm-*`: `llvm-profdata`, `llvm-ar`, `llvm-objcopy`, `llvm-objdump`
  (from `xcrun --sdk macosx --find ...`)

Install the `llvm-tools` component once if `xcrun` cannot find the
`llvm-*` tools:

```sh
xcode-select --install
# or, if you have a full Xcode.app:
sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
```

The CLI reports the same set of tools; the macOS slice uses the host
toolchain from `xcrun`.

## Cross-target builds on this host

The workspace builds on macOS with stable Rust. Add the Apple targets:

```sh
rustup target add aarch64-apple-darwin x86_64-apple-darwin
cargo build --release --target aarch64-apple-darwin
cargo build --release --target x86_64-apple-darwin
```

The macOS slices of the fixtures are built by `tests/fixtures/src/build-other.sh`
and `tests/fixtures/src/build.sh`. They use the host SDK toolchain for the
macOS slices and zig cc for the Linux slices:

```sh
bash tests/fixtures/src/build-other.sh
```

- macOS slices: `clang` from `xcrun` with `-mmacosx-version-min=11.0` and
  `-isysroot $(xcrun --sdk macosx --show-sdk-path)`.
- Linux slices: `zig cc` (`python3 -m pip install ziglang`).
- Universal macOS binary: `llvm-lipo -create macos-arm64 macos-x64 -output macos-universal`.

## Cross-compiling AnyOS itself

`anyos` is the Windows guest runtime; it targets Windows, and this host is the
development machine. The macOS plan is:

- Build `anyos` as an `x86_64-apple-darwin` binary so Rosetta 2 can translate
  the guest code along with AnyOS (the way CrossOver/Wine work today).
- For ARM64 Windows guests, measure whether `x18` survives on macOS before
  deciding whether ARM64 Windows code can run natively.

## Known macOS constraints

- 16 KiB pages: Apple Silicon macOS uses 16 KiB pages; the other supported
  hosts use 4 KiB. The CLI reports pages that would mix section protections.
- W^X: executable memory on Apple Silicon needs `MAP_JIT` and per-thread
  write/execute switching. Signed builds also need the JIT entitlements.
- TEB access: Windows x64 code reads its TEB through `gs`; macOS owns the
  `gs` base and points it at pthread thread-specific data.
- No 32-bit processes: macOS has no 32-bit process support, so 32-bit x86
  guests need 32-bit-on-64 thunking or translation.
