## Building the macOS launcher (Xcode-free)

`anyos/Launcher` is a SwiftPM package with no `.xcodeproj`. It uses the macOS
SDK from Xcode Command Line Tools, so a full Xcode.app install is not required.

```sh
cd anyos/Launcher
xcodebuild -scheme AnyOSLauncher -configuration Debug
```

The scheme produces `build/AnyOSLauncher.app`. To bundle the `anyos` binary and
the guest programs:

```sh
# from the repository root
python3 scripts/bundle_launcher.py
```

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
