# AnyOS 0.2.0

AnyOS is a standalone Rust runtime that runs programs built for one OS on
another, starting with Windows programs on macOS (then Linux and ChromeOS).
It does not use Wine and does not need a VM. Milestones 1 and 2 are done: it
can inspect Windows PE files and load them into an emulated guest process
(map, relocate, bind imports, set up TLS), and it recognises Linux (ELF) and
macOS (Mach-O) programs and says how it would run them, plus it keeps per-
program network redirects. It cannot execute guest code yet.

## What's new

- `anyos/Launcher`: a self-contained macOS app bundle that runs a guest
  program without Xcode or a runtime install on the user's machine. The app
  ships its own copy of the `anyos` binary, so the end user installs
  nothing.
- `scripts/bundle_launcher.py`: builds the `.app` from the bundled `anyos`
  binary and the fixture programs.
- `docs/macos-builds.md`: macOS SDK toolchain (`xcrun` clang/ld/llvm-lipo/
  llvm-dlltool), cross-target build steps, and known macOS constraints.
- `CONTRIBUTING.md`: workspace contract, SwiftPM launcher build steps, and
  the Xcode-free launcher build.
- `.github/workflows/build-macos.yml`: GitHub Actions CI that builds
  `anyos` for macOS (universal2), builds the launcher, and packages the
  app. No local Mac needed.
- `anyos/Launcher`: SwiftPM package (no `.xcodeproj`) with console and
  visual launch modes. `xcodebuild -scheme AnyOSLauncher` uses the macOS
  SDK from Xcode Command Line Tools only.

## What still does not work yet

- Guest execution is not implemented; `anyos run` still needs the
  corresponding milestone.
- The `.app` is not yet signed or notarized. Code signing and notarization
  require an Apple Developer ID (paid program). Until then the app imports
  with a warning on macOS.
