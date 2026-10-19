## Building the macOS launcher (Xcode-free)

`anyos/Launcher` is a SwiftPM package with no `.xcodeproj`. It uses the macOS
SDK from Xcode Command Line Tools, so a full Xcode.app install is not required.

```sh
cd anyos/Launcher
swift build
swift test
```

`swift build` produces `.build/debug/AnyOSLauncher`; `swift test` runs the
launcher's XCTest suite, which covers argument splitting, host-path handling
and locate-the-bundled-`anyos` logic without needing the app bundle. The
`.app` around that executable is assembled by `scripts/bundle_launcher.py`.

`swift build` and `swift test` need only Xcode Command Line Tools. They are
what CI runs on `macos-latest`; an Xcode project and a full Xcode.app install
are not required.

`scripts/bundle_launcher.py` copies `target/release/anyos` into
`anyos/Launcher/Contents/MacOS/`, copies the fixtures into
`Contents/Resources/binary/`, and stamps the `Info.plist` version. It is a
plain Python script with no third-party dependencies.

## Testing the launcher on macOS

```sh
open -a build/AnyOSLauncher -- anyos/tests/fixtures/bin/x86_64/hello_console.exe
```

or, from inside the app, drag `hello_console.exe` onto the window. Both paths
run through the bundled `Contents/MacOS/anyos`.
