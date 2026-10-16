#!/usr/bin/env bash
# Builds the interpreter self-test (selftest.h) for Windows x64 and x86, and its
# expected output by running the same code natively. Needs an x86-64 Linux host
# with clang, lld-link and llvm-dlltool (no 32-bit libc needed: the native build
# is freestanding).
set -euo pipefail
cd "$(dirname "$0")"
SRC=$PWD
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
CFLAGS="-O2 -ffreestanding -fno-stack-protector -fno-builtin -nostdlib -Wall -Werror"

sed 's/@[0-9]*$//' kernel32.def >"$WORK/k64.def"
llvm-dlltool -m i386:x86-64 -d "$WORK/k64.def" -l "$WORK/k64.lib"
llvm-dlltool -m i386 -d kernel32.def -l "$WORK/k32.lib" -k

# arch  clang-triple             lld-machine  native-flag  import-lib
for t in "x64 x86_64-pc-windows-msvc x64 -m64 k64" "x86 i686-pc-windows-msvc x86 -m32 k32"; do
  read -r arch triple lm m lib <<<"$t"
  extra=()
  [ "$arch" = x86 ] && extra=(/safeseh:no)
  clang --target="$triple" $CFLAGS -c selftest.c -o "$WORK/st-$arch.obj"
  lld-link /nologo /machine:"$lm" /nodefaultlib /dynamicbase /Brepro "${extra[@]}" \
    /subsystem:console /entry:mainCRTStartup "$WORK/st-$arch.obj" "$WORK/$lib.lib" \
    /out:"../bin/$arch/selftest.exe"
  clang "$m" $CFLAGS -static -fno-pie -I"$SRC" selftest_native.c -o "$WORK/native-$arch"
  "$WORK/native-$arch" >"../bin/$arch/selftest.expected"
done
ls -l ../bin/x64/selftest.* ../bin/x86/selftest.*
