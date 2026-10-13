#!/usr/bin/env bash
# Rebuilds the non-Windows fixtures in ../bin/other with zig cc
# (`python3 -m pip install ziglang`) and llvm-lipo for the universal binary.
# Run from a directory outside network mounts (zig needs a normal cwd).
set -euo pipefail
SRC="$(cd "$(dirname "$0")" && pwd)"
OUT="$SRC/../bin/other"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cp "$SRC/hello_unix.c" "$WORK/"
cd "$WORK"
ZIG=(python3 -m ziglang cc -Os -s hello_unix.c)
"${ZIG[@]}" -target x86_64-linux-gnu -o "$OUT/linux-x64"
"${ZIG[@]}" -target aarch64-linux-gnu -o "$OUT/linux-arm64"
"${ZIG[@]}" -target x86_64-linux-musl -o "$OUT/linux-x64-static"
"${ZIG[@]}" -target aarch64-macos -o "$OUT/macos-arm64"
"${ZIG[@]}" -target x86_64-macos -o "$OUT/macos-x64"
llvm-lipo -create "$OUT/macos-arm64" "$OUT/macos-x64" -output "$OUT/macos-universal"
ls -l "$OUT"
