## Bundling the app for the user

The `.app` ships with its own copy of the `anyos` binary, so the user does not
install anything. The build flow is:

1. CI builds `anyos` for macOS on a macOS host and puts it at
   `Contents/MacOS/anyos`.
2. `Contents/Resources/binary/` is filled from `tests/fixtures/bin/other/` (the
   Windows PE and macOS Mach-O fixtures the app can launch).
3. `xcodebuild -scheme AnyOSLauncher` builds the Swift helper that finds and
   runs the bundled binary.

The launcher is a SwiftPM package, not an Xcode project, so it has no
`.xcodeproj` and no Xcode project file. `xcodebuild -scheme AnyOSLauncher`
uses the macOS SDK from Xcode Command Line Tools, so a full Xcode.app install is
not required.
