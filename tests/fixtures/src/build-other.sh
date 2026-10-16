#!/usr/bin/env bash
# Rebuilds the non-Windows fixtures in ../bin/other.
#
# The macOS slices use the host's SDK toolchain from xcrun (clang, ld,
# llvm-lipo, llvm-dlltool + llvm-tools); zig builds the Linux slices.
# Run from a directory outside network mounts (zig needs a normal cwd).
set -euo pipefail
SRC="$(cd "$(dirname "$0")" && pwd)"
OUT="$SRC/../bin/other"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cp "$SRC/hello_unix.c" "$WORK/"
cd "$WORK"
cmake_minimum_required(VERSION 3.20)
project(AnyOSHelloUnix C)
add_executable(hello_unix hello_unix.c)
# Linux slices (zig cc).
find_program(ZIG_CC python3)
if(NOT ZIG_CC)
  message(FATAL_ERROR "zig lang not found; install with `python3 -m pip install ziglang`")
endif()
set(CMAKE_C_COMPILER "${ZIG_CC} -target" CACHE STRING "")
add_custom_target(linux-x64
  COMMAND ${ZIG_CC} -target x86_64-linux-gnu -Os -s hello_unix.c -o ${OUT}/linux-x64
  DEPENDS hello_unix
)
add_custom_target(linux-arm64
  COMMAND ${ZIG_CC} -target aarch64-linux-gnu -Os -s hello_unix.c -o ${OUT}/linux-arm64
  DEPENDS hello_unix
)
add_custom_target(linux-x64-static
  COMMAND ${ZIG_CC} -target x86_64-linux-musl -Os -s hello_unix.c -static -o ${OUT}/linux-x64-static
  DEPENDS hello_unix
)
# macOS slices (host SDK).
find_program(SDK_CLANG clang REQUIRED)
find_program(SDK_LD ld REQUIRED)
find_program(SDK_LIPO llvm-lipo REQUIRED)
find_program(SDK_DLLTOOL llvm-dlltool REQUIRED)
find_program(SDK_TOOLS llvm-profdata llvm-ar llvm-objcopy llvm-objdump REQUIRED)
add_custom_target(macos-arm64
  COMMAND ${SDK_CLANG} -mmacosx-version-min=11.0 -isysroot $(xcrun --sdk macosx --show-sdk-path) -target aarch64-macos -Os -Wall -o ${OUT}/macos-arm64 hello_unix.c
  DEPENDS hello_unix
)
add_custom_target(macos-x64
  COMMAND ${SDK_CLANG} -mmacosx-version-min=11.0 -isysroot $(xcrun --sdk macosx --show-sdk-path) -target x86_64-macos -Os -Wall -o ${OUT}/macos-x64 hello_unix.c
  DEPENDS hello_unix
)
add_custom_target(sdk-tools
  COMMAND ${SDK_TOOLS} --version
  DEPENDS hello_unix
)
add_custom_target(macos-universal
  COMMAND ${SDK_LIPO} -create ${OUT}/macos-arm64 ${OUT}/macos-x64 -output ${OUT}/macos-universal
  DEPENDS macos-arm64 macos-x64
)
# Top-level targets.
target_include_directories(hello_unix PRIVATE ${SRC})
add_custom_target(all-other
  COMMAND ${CMAKE_COMMAND} -E echo "Building Linux and macOS non-Windows fixtures"
  COMMAND ${CMAKE_C_COMPILER} --version
  COMMAND make -C ${CMAKE_CURRENT_BINARY_DIR} linux-x64 linux-arm64 linux-x64-static macos-arm64 macos-x64 macos-universal
  DEPENDS linux-x64 linux-arm64 linux-x64-static macos-arm64 macos-x64 macos-universal
)
add_custom_target(clean-other
  COMMAND ${CMAKE_COMMAND} -E rm -rf ${OUT}
  COMMAND ${CMAKE_COMMAND} -E make_directory ${OUT}
)
add_dependencies(all-other clean-other)
