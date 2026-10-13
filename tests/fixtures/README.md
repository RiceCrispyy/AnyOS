# Test fixtures

`bin/<arch>/` holds tiny Windows images built from `src/` by `src/build.sh`
(clang + lld-link + llvm-dlltool, no Windows SDK). `<arch>` is `x64`, `arm64` or
`x86`. The builds are reproducible (`/Brepro`), so rebuilding gives identical bytes.

| File | What it exercises |
|---|---|
| `hello_console.exe` | console subsystem, kernel32 imports, pointer table (base relocations), hand-built TLS directory with one callback |
| `hello_gui.exe` | GUI subsystem, delay-loaded `user32.dll` |
| `exports_dll.dll` | DLL entry point, named exports, a NONAME ordinal export (`@7`), a data export, ordinal base 7 |
| `uses_dll.exe` | imports from `exports_dll.dll` by name and by ordinal |

`bin/other/` holds the same tiny C program (`src/hello_unix.c`) built for other
OSes by `src/build-other.sh` (zig cc, llvm-lipo): `linux-x64` and `linux-arm64`
(glibc, dynamic), `linux-x64-static` (musl, static), `macos-arm64`, `macos-x64`
and `macos-universal` (both slices). They exercise `anyos-format` detection and
planning.

`third_party/` contains one small, unmodified, MIT-licensed .NET assembly used as
managed-code input. None of these files are ever executed by the tests.
