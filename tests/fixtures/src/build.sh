#!/usr/bin/env bash
# Rebuilds the PE fixtures with clang + lld-link (no Windows SDK needed).
# Output goes to ../bin/<arch>/ so that DLL names match what the executables import. Requires clang, lld-link and llvm-dlltool (LLVM >= 15).
set -euo pipefail
cd "$(dirname "$0")"
OUT=../bin
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$OUT"

# arch-name  clang-triple              dlltool-machine  lld-link-machine
TARGETS=(
  "x64   x86_64-pc-windows-msvc  i386:x86-64 x64"
  "arm64 aarch64-pc-windows-msvc arm64       arm64"
  "x86   i686-pc-windows-msvc    i386        x86"
)
CFLAGS="-O1 -ffreestanding -fno-stack-protector -fno-builtin -nostdlib -Wall -Werror"

for t in "${TARGETS[@]}"; do
  read -r arch triple dlm lm <<<"$t"
  for lib in kernel32 user32; do
    if [ "$arch" = x86 ]; then
      # stdcall: keep @N decoration in the import lib, strip it from the DLL name (-k).
      llvm-dlltool -m "$dlm" -d "$lib.def" -l "$WORK/$lib-$arch.lib" -k
    else
      sed 's/@[0-9]*$//' "$lib.def" >"$WORK/$lib-$arch.def"
      llvm-dlltool -m "$dlm" -d "$WORK/$lib-$arch.def" -l "$WORK/$lib-$arch.lib"
    fi
  done
  for src in hello_console hello_gui exports_dll uses_dll; do
    clang --target="$triple" $CFLAGS -c "$src.c" -o "$WORK/$src-$arch.obj"
  done
  out="$OUT/$arch"
  mkdir -p "$out"
  common=(/nologo /machine:"$lm" /nodefaultlib /dynamicbase /Brepro)
  [ "$arch" = x86 ] && common+=(/safeseh:no)
  lld-link "${common[@]}" /subsystem:console /entry:mainCRTStartup \
    "$WORK/hello_console-$arch.obj" "$WORK/kernel32-$arch.lib" /out:"$out/hello_console.exe"
  lld-link "${common[@]}" /subsystem:windows /entry:WinMainCRTStartup \
    /delayload:user32.dll \
    "$WORK/hello_gui-$arch.obj" "$WORK/kernel32-$arch.lib" "$WORK/user32-$arch.lib" /out:"$out/hello_gui.exe"
  lld-link "${common[@]}" /dll /entry:_DllMainCRTStartup /def:exports_dll.def \
    "$WORK/exports_dll-$arch.obj" /out:"$out/exports_dll.dll" /implib:"$WORK/exports_dll-$arch.lib"
  lld-link "${common[@]}" /subsystem:console /entry:mainCRTStartup \
    "$WORK/uses_dll-$arch.obj" "$WORK/exports_dll-$arch.lib" "$WORK/kernel32-$arch.lib" /out:"$out/uses_dll.exe"
done
ls -lR "$OUT"
