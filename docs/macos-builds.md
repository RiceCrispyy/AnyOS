## Bundling the app for the user

The `.app` ships with its own copy of the `anyos` binary, so the user does not
install anything. The build flow is:

1. `cargo build --release` on macOS produces the embedded `anyos` binary.
2. Run `scripts/bundle_launcher.py` from the repository root on macOS to fill
   `anyos/Launcher/Contents/MacOS/anyos`, `Contents/Resources/binary/` (from
   `tests/fixtures/bin/other/`), and `Contents/Resources/doc/`.
3. `swift build` (in `anyos/Launcher`) builds the Swift helper that finds and
   runs the bundled binary; `swift test` runs its tests.

The launcher is a SwiftPM package, not an Xcode project, so it has no
`.xcodeproj` and no Xcode project file. `swift build` and `swift test` use the
macOS SDK from Xcode Command Line Tools, so a full Xcode.app install is not
required.

```sh
cd anyos/Launcher
swift build
swift test
```

`swift build` produces `.build/debug/AnyOSLauncher`. `scripts/bundle_launcher.py`
assembles the surrounding `AnyOSLauncher.app`.
