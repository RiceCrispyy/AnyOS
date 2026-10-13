# AnyOS — handoff from the session that fixed the Windows CLI error

Date: 2026-10-13  (local MINGW64 / Windows x86_64 host, Rust 1.99.0 stable)
Branch: main, ahead of origin/main by 2 commits (the two fixes below, not yet pushed).

## What this project is
A standalone Rust runtime that runs programs built for one OS on another, starting
with Windows on macOS (then Linux and ChromeOS). No Wine, no VM.

Status (READ THIS IF YOU CUT A RELEASE): milestones 1 and 2 are done.
"anyos run" the CLI cannot execute guest code yet" — at line 8 of README.md.
This session landed fixes for milestone 3 (execution) on Windows only.

Workspace layout:
- crates/anyos-pe, anyos-memory, anyos-loader, anyos-net, anyos-format,
  anyos-elf, anyos-linux, anyos-exec
- apps/anyos-cli  →  the `anyos` binary
- .github/workflows/ci.yml  →  linux x86_64, tests + clippy + fmt
- tests/fixtures/bin/{x86,x86_64,other}/  →  tiny test programs
- docs/architecture.md  →  roadmap / interfaces

## What changed in this session (uncommitted, ready to commit)
Two files on disk:

1. crates/anyos-exec/src/native.rs  (the Windows VEH SYSCALL-trap handler)
   - Rip offset bug: int3 = rip - 1 was wrong; Windows can deliver Rip pointing at
     the 0xCC itself. Now accept rip or rip-1, whichever is in the patched set.
   - Exit handling: exit/exit_group now really stops the guest thread via ExitThread,
     instead of resuming execution with no stored exit code. Drop GUEST lock first
     to avoid deadlocking the joining main thread.
   - TLS: arch_prctl(ARCH_SET_FS) writes the guest FS base to the real Windows
     thread via wrfsbase (guarded by IsProcessorFeaturePresent(PF_RDWRFSGSBASE_AVAILABLE)).
   - Real↔shadow memory sync: before each syscall, copy real memory → shadow
     (so the dispatcher can read guest buffers). After each syscall, copy
     shadow → real (so syscall outputs reach the guest). Stack is only synced
     from rsp upward because the VEH handler itself runs on the guest stack.
   - Clippy: added #[allow(clippy::upper_case_acronyms)] on the Win32 type aliases.

2. crates/anyos-linux/src/syscall.rs  (merged PR #3 + fixed its Windows compile error)
   - PR #3 added syscall coverage: open/openat, close, lseek, stat/lstat/newfstatat,
     ioctl, access, fcntl, getcwd, readlink, arch_prctl, getdents64, clock_gettime,
     plus getuid/getgid, set_tid_address, and extended read/write/writev for
     GuestFd::File.
   - PR #3's getdents64 was Linux-only and made `path` a `()` on non-Linux,
     breaking `&path` / AsRef<Path> on Windows. Extracted into proc_fd_path(fd)
     with a cfg split so non-Linux returns ENOSYS cleanly.

Result: the workspace builds clean, cargo test --workspace passes, clippy is clean,
fmt is clean, and `anyos run` the static Linux ELF fixture actually runs and prints
"hello from anyos fixture" with exit 0 on this Windows host.

Untracked:  .freebuff/project-id  (session metadata, safe to ignore)

## Verified behavior (on this host)
- anyos inspect <pe-exe>            ✓  (PE detection, header summary)
- anyos inspect --all <pe-exe>      ✓  (sections, imports, exports)
- anyos inspect --json <pe-exe>     ✓  (machine-readable, parseable)
- anyos inspect --host macos-arm64 <pe-exe>  ✓
- anyos load --imports <pe-exe>     ✓  (dry-run: map, relocate, bind, TLS)
- anyos load --rebase <pe-exe>      ✓  (dry-run: forced relocation)
- anyos redirect add / list / test / remove  ✓  (writes <prog>.anyos.toml)
- anyos run tests/fixtures/bin/other/linux-x64-static  ✓
      prints "hello from anyos fixture", exits 0
- anyos run tests/fixtures/bin/other/linux-x64  ✗  (expected: /lib64/ld-linux-x86-64.so.2 missing on this host)
- anyos run <macos binary>          ✗  (not implemented yet: Mach-O execution)

NOTE about the dynamic Linux ELF failure: it fails with os error 3 ("path not found")
because the PT_INTERP path /lib64/ld-linux-x86-64.so.2 does not exist on this
Windows host. This is not a bug in the code; the interpreter binary is simply not
present here. On a Linux host, or once a bundled interp is shipped, it should work.

NOTE about macOS: only inspection works. AnyOS detects Mach-O and reports how it
would run it, but does not execute Mach-O yet.

## Build artifacts
- target/release/anyos.exe   1.7 MB   version 0.1.0   (PolyForm Noncommercial 1.0.0)
- No CHANGELOG.md or RELEASE_NOTES.md exist yet.

## CI
- Latest successful CI: PR #2 (main), PR #1 (main).
- Latest failing CI: PR #3 (claude/project-thread-0hfm2i) — the Windows compile error
  in anyos-linux/src/syscall.rs. This session fixed that error, so PR #3's branch
  would now pass on Windows if retried (not verified, but plausible).

## If you want to cut a release
Remember README.md still says "cannot execute guest code yet." If you cut a 0.1.0
tag now, the honest scope is: inspect + load + redirect + anyos run for static Linux
ELF on Windows. macOS execution and dynamic Linux ELF execution are not ready.
Do not claim "runs programs from any OS on any OS" in release notes until macOS
execution and dynamic Linux ELF execution actually work.

## Next milestone (what remains before "any OS on any OS" is honest)
1. Bundle / ship a Linux ELF dynamic loader (PT_INTERP interpreter) so dynamic ELF
   programs run without a host /lib64 present.
2. Implement Mach-O execution for macOS (currently only inspection).
3. Expand Windows syscall coverage beyond what PR #3 added (msync, epoll, signals,
   thread APIs, etc.) as real guest programs need them.
4. Test against real guest programs (the corpus test under tests/fixtures is the path
   for that; see README "Tests" section and the ANYOS_CORPUS env var).

## Git map
- origin/main                        latest stable published head
- origin/claude/project-thread-0hfm2i   Claude's working branch (PR #1, #2, #3)
- main on this host: 560785d Merge remote-tracking branch...  (PR #3 merge)
                       2e4af90 anyos-exec: fix Windows VEH off-by-one...  (this session)
                       d4a7089 anyos-exec: fix ETXTBSY race...  (Claude, Linux-only fix)
                       a067f57 anyos-linux: broader syscall coverage...  (Claude, PR #3)
                       aeef5f8 Merge pull request #2...  (main)

## Session boundary
This workspace is a full checkout of RiceCrispyy/AnyOS with .git. The two fix files
are unstaged and uncommitted and ready to be committed whenever you want. No secrets,
no deploy, no push has been done.
