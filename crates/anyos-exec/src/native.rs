//! Platform-specific ELF runner.

use anyos_elf::ElfArch;
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("ELF loading failed: {0}")]
    Elf(#[from] anyos_elf::ElfError),
    #[error("not supported on this host (can only run x86-64 Linux ELF on an x86-64 host)")]
    HostMismatch,
    #[error("memory: {0}")]
    Memory(#[from] anyos_memory::MemoryError),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Platform(String),
}

/// Options controlling a guest run.
pub struct RunOptions<'a> {
    /// Arguments for the guest program (argv[0] = program name).
    pub argv: &'a [&'a str],
    /// Environment variables as "KEY=VALUE" strings.
    pub envp: &'a [&'a str],
}

/// Result of running a guest program.
#[derive(Debug, Clone, Serialize)]
pub struct RunResult {
    pub exit_code: i32,
}

/// Run the ELF at `bytes` and return when it exits.
pub fn run(bytes: &[u8], opts: &RunOptions<'_>) -> Result<RunResult, RunError> {
    let summary = anyos_elf::summarise(bytes)?;
    if summary.arch != ElfArch::X86_64 {
        return Err(RunError::HostMismatch);
    }

    #[cfg(target_os = "windows")]
    return windows_run(bytes, opts);

    #[cfg(target_os = "linux")]
    return linux_direct(bytes, opts);

    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    return Err(RunError::Platform(
        "execution not supported on this OS".into(),
    ));
}

// ═══════════════════════════════════════════════════════════════════════════
// Windows — native execution with SYSCALL patching + VEH
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(target_os = "windows")]
fn windows_run(bytes: &[u8], opts: &RunOptions<'_>) -> Result<RunResult, RunError> {
    use anyos_linux::{
        stack::{build as build_stack, StackParams},
        syscall::{dispatch, SyscallArgs, SyscallResult},
        LinuxProcess,
    };
    use anyos_memory::{round_up, AddressSpace, Protection, PAGE_SIZE};
    use goblin::elf::{program_header, Elf};
    use std::collections::HashMap;
    use std::sync::Mutex;

    // ── Windows API declarations ──────────────────────────────────────────

    // Names mirror the Win32 API.
    #[allow(clippy::upper_case_acronyms)]
    type PVOID = *mut core::ffi::c_void;
    #[allow(clippy::upper_case_acronyms)]
    type ULONG = u32;
    #[allow(clippy::upper_case_acronyms)]
    type DWORD = u32;
    #[allow(clippy::upper_case_acronyms)]
    type BOOL = i32;
    #[allow(clippy::upper_case_acronyms)]
    type LONG = i32;

    #[repr(C)]
    struct ExceptionRecord {
        code: DWORD,
        flags: DWORD,
        next: PVOID,
        address: PVOID,
        nparam: ULONG,
        info: [usize; 15],
    }

    /// Minimal overlay for the x64 CONTEXT structure.
    /// rax=0x78, rcx=0x80, rdx=0x88, rbx=0x90, rsp=0x98, rbp=0xa0,
    /// rsi=0xa8, rdi=0xb0, r8=0xb8, r9=0xc0, r10=0xc8, rip=0xf8.
    #[repr(C, align(16))]
    struct Context([u8; 0x4d0]);

    impl Context {
        fn read64(&self, off: usize) -> u64 {
            u64::from_le_bytes(self.0[off..off + 8].try_into().unwrap())
        }
        fn write64(&mut self, off: usize, v: u64) {
            self.0[off..off + 8].copy_from_slice(&v.to_le_bytes());
        }
        fn rax(&self) -> u64 {
            self.read64(0x78)
        }
        fn rdi(&self) -> u64 {
            self.read64(0xb0)
        }
        fn rsi(&self) -> u64 {
            self.read64(0xa8)
        }
        fn rdx(&self) -> u64 {
            self.read64(0x88)
        }
        fn r10(&self) -> u64 {
            self.read64(0xc8)
        }
        fn r8(&self) -> u64 {
            self.read64(0xb8)
        }
        fn r9(&self) -> u64 {
            self.read64(0xc0)
        }
        fn rip(&self) -> u64 {
            self.read64(0xf8)
        }
        fn rsp(&self) -> u64 {
            self.read64(0x98)
        }
        fn set_rax(&mut self, v: u64) {
            self.write64(0x78, v);
        }
        fn set_rip(&mut self, v: u64) {
            self.write64(0xf8, v);
        }
    }

    #[repr(C)]
    struct ExceptionPointers {
        record: *mut ExceptionRecord,
        context: *mut Context,
    }

    extern "system" {
        fn VirtualAlloc(addr: PVOID, size: usize, atype: DWORD, prot: DWORD) -> PVOID;
        fn VirtualFree(addr: PVOID, size: usize, ftype: DWORD) -> BOOL;
        fn AddVectoredExceptionHandler(
            first: ULONG,
            handler: unsafe extern "system" fn(*mut ExceptionPointers) -> LONG,
        ) -> PVOID;
        fn RemoveVectoredExceptionHandler(handle: PVOID) -> ULONG;
        /// Terminates *only* the calling thread (the guest thread); other
        /// threads in the process are unaffected. Used to honour guest
        /// `exit`/`exit_group` without resuming guest code.
        fn ExitThread(code: u32) -> !;
    }

    const MEM_COMMIT: DWORD = 0x1000;
    const MEM_RESERVE: DWORD = 0x2000;
    const MEM_RELEASE: DWORD = 0x8000;
    const PAGE_EXECUTE_READWRITE: DWORD = 0x40;
    const PAGE_READWRITE: DWORD = 0x04;
    const EXCEPTION_BREAKPOINT: DWORD = 0x80000003;
    const EXCEPTION_CONTINUE_EXECUTION: LONG = -1;
    const EXCEPTION_CONTINUE_SEARCH: LONG = 0;

    // ── per-process global (active while the guest runs) ─────────────────

    struct GuestState {
        /// Addresses (of the INT3 byte) that came from patched SYSCALLs.
        patched: HashMap<u64, ()>,
        proc: Option<LinuxProcess>,
        exit_code: Option<i32>,
        /// Real-memory regions (base, len) mirrored in the shadow address
        /// space. Synced real→shadow before each syscall (the guest writes
        /// its own stack/image directly) and shadow→real after it (syscalls
        /// write output buffers into the shadow space).
        regions: Vec<(u64, u64)>,
        /// The guest stack (base, size). Write-back is limited to
        /// [saved_rsp, top): the VEH handler runs on this very stack, just
        /// below the guest's rsp, so copying the whole region would
        /// overwrite the exception dispatch's own frames.
        stack: (u64, u64),
    }

    // Safety: we use this only from a single guest thread.
    unsafe impl Send for GuestState {}
    unsafe impl Sync for GuestState {}

    static GUEST: std::sync::LazyLock<Mutex<Option<GuestState>>> =
        std::sync::LazyLock::new(|| Mutex::new(None));

    unsafe extern "system" fn veh(ptrs: *mut ExceptionPointers) -> LONG {
        let rec = &*(*ptrs).record;
        let ctx = &mut *(*ptrs).context;
        if rec.code != EXCEPTION_BREAKPOINT {
            let fault = rec.info[1];
            let insn: Vec<String> = (ctx.rip()..ctx.rip() + 8)
                .map(|a| format!("{:02x}", unsafe { std::ptr::read_volatile(a as *const u8) }))
                .collect();
            eprintln!(
                "[veh] exception {:#x} at {:?} rip={:#x} fault_addr={:#x} insn={}",
                rec.code,
                rec.address,
                ctx.rip(),
                fault,
                insn.join(" ")
            );
            return EXCEPTION_CONTINUE_SEARCH;
        }
        // Depending on how the breakpoint was delivered, Rip may point at the
        // 0xCC itself or one byte past it. Accept either, and remember which
        // so we resume at the instruction after the INT3/NOP pair.
        let rip = ctx.rip();
        let mut guard = GUEST.lock().unwrap();
        let state = match guard.as_mut() {
            Some(s) => s,
            None => return EXCEPTION_CONTINUE_SEARCH,
        };
        let int3 = if state.patched.contains_key(&rip) {
            rip
        } else if rip > 0 && state.patched.contains_key(&(rip - 1)) {
            rip - 1
        } else {
            return EXCEPTION_CONTINUE_SEARCH;
        };
        // The patched pair is [0xCC, 0x90] where the syscall's `0f 05` was;
        // resume at int3 + 2 either way.
        let resume = int3 + 2;
        let args = SyscallArgs {
            nr: ctx.rax(),
            a1: ctx.rdi(),
            a2: ctx.rsi(),
            a3: ctx.rdx(),
            a4: ctx.r10(),
            a5: ctx.r8(),
            a6: ctx.r9(),
        };
        // The guest writes to real memory; the dispatcher reads the shadow
        // copy. Pull real → shadow first so input buffers (iov arrays on the
        // stack, string arguments in .data, …) are visible to dispatch().
        for &(base, len) in &state.regions {
            let src = std::slice::from_raw_parts(base as *const u8, len as usize);
            let _ = state.proc.as_mut().unwrap().memory.write_raw(base, src);
        }
        let exit = match dispatch(args, state.proc.as_mut().unwrap()) {
            SyscallResult::Value(v) => {
                ctx.set_rax(v);
                ctx.set_rip(resume);
                None
            }
            SyscallResult::Exit(code) => {
                state.exit_code = Some(code);
                Some(code)
            }
            SyscallResult::Unimplemented(nr) => {
                eprintln!("[anyos] unimplemented syscall {nr}");
                ctx.set_rax((-38i64) as u64); // -ENOSYS
                ctx.set_rip(resume);
                None
            }
        };
        // …and push shadow → real so syscall outputs (read, getcwd, stat, …)
        // land in the memory the guest actually reads.
        //
        // The stack gets special treatment: this handler runs on the guest
        // thread's real stack, a few KB below the guest's rsp (Windows
        // dispatches exceptions on the faulting thread's stack). Copying the
        // whole 8MB region back would smash the exception dispatch frames
        // mid-handler, so only [rsp, top) is written back.
        let rsp = ctx.rsp();
        let (stack_base, stack_size) = state.stack;
        for &(base, len) in &state.regions {
            if base == stack_base {
                continue;
            }
            let mut buf = vec![0u8; len as usize];
            if state
                .proc
                .as_ref()
                .unwrap()
                .memory
                .read_raw(base, &mut buf)
                .is_ok()
            {
                std::ptr::copy_nonoverlapping(buf.as_ptr(), base as *mut u8, len as usize);
            }
        }
        if (stack_base..stack_base + stack_size).contains(&rsp) {
            let len = (stack_base + stack_size - rsp) as usize;
            let mut buf = vec![0u8; len];
            if state
                .proc
                .as_ref()
                .unwrap()
                .memory
                .read_raw(rsp, &mut buf)
                .is_ok()
            {
                std::ptr::copy_nonoverlapping(buf.as_ptr(), rsp as *mut u8, len);
            }
        }
        // exit/exit_group: stop this guest thread for real. Releasing the
        // lock first is essential — the main thread reads GUEST after the
        // guest thread joins and would otherwise deadlock.
        drop(guard);
        if let Some(code) = exit {
            ExitThread(code as u32);
        }
        EXCEPTION_CONTINUE_EXECUTION
    }

    unsafe fn valloc(vaddr: u64, size: usize, exec: bool) -> Option<*mut u8> {
        let prot = if exec {
            PAGE_EXECUTE_READWRITE
        } else {
            PAGE_READWRITE
        };
        let hint = if vaddr == 0 {
            std::ptr::null_mut()
        } else {
            vaddr as PVOID
        };
        let p = VirtualAlloc(hint, size, MEM_COMMIT | MEM_RESERVE, prot);
        if p.is_null() {
            None
        } else {
            Some(p as *mut u8)
        }
    }

    // ── parse + load ───────────────────────────────────────────────────────

    let elf = Elf::parse(bytes).map_err(|e| RunError::Elf(anyos_elf::ElfError::Parse(e)))?;
    let is_pie = elf.header.e_type == goblin::elf::header::ET_DYN;

    let loads: Vec<_> = elf
        .program_headers
        .iter()
        .filter(|ph| ph.p_type == program_header::PT_LOAD && ph.p_memsz > 0)
        .collect();
    if loads.is_empty() {
        return Err(RunError::Platform("no PT_LOAD segments".into()));
    }

    let min_page = loads
        .iter()
        .map(|ph| ph.p_vaddr & !(PAGE_SIZE - 1))
        .min()
        .unwrap();
    let max_end = loads
        .iter()
        .map(|ph| round_up(ph.p_vaddr + ph.p_memsz, PAGE_SIZE))
        .max()
        .unwrap();
    let image_size = (max_end - min_page) as usize;

    let load_bias = unsafe {
        let hint = if is_pie { 0 } else { min_page };
        let p = valloc(hint, image_size, true)
            .ok_or_else(|| RunError::Platform("VirtualAlloc for image failed".into()))?;
        p as u64 - min_page
    };

    unsafe {
        for ph in &loads {
            let dst = (load_bias + ph.p_vaddr) as *mut u8;
            let off = ph.p_offset as usize;
            let len = ph.p_filesz as usize;
            let src = bytes
                .get(off..off + len)
                .ok_or_else(|| RunError::Platform("segment past EOF".into()))?;
            std::ptr::copy_nonoverlapping(src.as_ptr(), dst, len);
        }
    }

    let entry = load_bias + elf.header.e_entry;

    // ── patch SYSCALL → INT3 NOP ───────────────────────────────────────────
    let mut patched: HashMap<u64, ()> = HashMap::new();
    unsafe {
        use goblin::elf::program_header::PF_X;
        for ph in &loads {
            if ph.p_flags & PF_X == 0 {
                continue;
            }
            let base = (load_bias + ph.p_vaddr) as *mut u8;
            let len = ph.p_filesz as usize;
            let seg = std::slice::from_raw_parts_mut(base, len);
            for i in 0..len.saturating_sub(1) {
                if seg[i] == 0x0f && seg[i + 1] == 0x05 {
                    seg[i] = 0xcc;
                    seg[i + 1] = 0x90;
                    patched.insert(base as u64 + i as u64, ());
                }
            }
        }
    }

    // ── build shadow AddressSpace for syscall memory operations ──────────
    let mut space = AddressSpace::new(0x1_0000, 0x7fff_ffff_f000, 2 << 30);
    unsafe {
        use goblin::elf::program_header::{PF_R, PF_W, PF_X};
        for ph in &loads {
            let map_base = (load_bias + ph.p_vaddr) & !(PAGE_SIZE - 1);
            let memsz = round_up((ph.p_vaddr % PAGE_SIZE) + ph.p_memsz, PAGE_SIZE);
            let prot = Protection {
                read: ph.p_flags & PF_R != 0,
                write: ph.p_flags & PF_W != 0,
                execute: ph.p_flags & PF_X != 0,
            };
            let _ = space.map(map_base, memsz, prot, "seg");
            let data_ptr = (load_bias + ph.p_vaddr) as *const u8;
            let data = std::slice::from_raw_parts(data_ptr, ph.p_filesz as usize);
            let _ = space.write_raw(load_bias + ph.p_vaddr, data);
        }
    }

    let initial_brk = round_up(load_bias + max_end, PAGE_SIZE);

    // Find the phdr vaddr.
    let phoff = elf.header.e_phoff;
    let phdr_vaddr = loads
        .iter()
        .find(|ph| phoff >= ph.p_offset && phoff < ph.p_offset + ph.p_filesz)
        .map(|ph| load_bias + ph.p_vaddr + (phoff - ph.p_offset))
        .unwrap_or(load_bias + phoff);

    // ── load interpreter if dynamic ────────────────────────────────────────
    // For dynamic ELFs, we load the PT_INTERP interpreter ELF into real memory
    // and jump to the interpreter's entry point instead of the main entry.
    let interp_load_base: u64;
    let jump_entry: u64;

    let interp_path = elf.program_headers.iter().find_map(|ph| {
        if ph.p_type == program_header::PT_INTERP {
            let start = ph.p_offset as usize;
            let end = (ph.p_offset + ph.p_filesz) as usize;
            bytes.get(start..end).map(|s| {
                let s = s.strip_suffix(b"\0").unwrap_or(s);
                String::from_utf8_lossy(s).into_owned()
            })
        } else {
            None
        }
    });

    if let Some(ref path) = interp_path {
        let interp_bytes = std::fs::read(path)?;
        let interp_elf =
            Elf::parse(&interp_bytes).map_err(|e| RunError::Elf(anyos_elf::ElfError::Parse(e)))?;
        let interp_loads: Vec<_> = interp_elf
            .program_headers
            .iter()
            .filter(|ph| ph.p_type == program_header::PT_LOAD && ph.p_memsz > 0)
            .collect();
        let interp_min = interp_loads
            .iter()
            .map(|ph| ph.p_vaddr & !(PAGE_SIZE - 1))
            .min()
            .unwrap_or(0);
        let interp_max = interp_loads
            .iter()
            .map(|ph| round_up(ph.p_vaddr + ph.p_memsz, PAGE_SIZE))
            .max()
            .unwrap_or(0);
        let interp_size = (interp_max - interp_min) as usize;
        let interp_bias = unsafe {
            let p = valloc(0, interp_size, true)
                .ok_or_else(|| RunError::Platform("VirtualAlloc for interpreter failed".into()))?;
            p as u64 - interp_min
        };
        unsafe {
            for ph in &interp_loads {
                let dst = (interp_bias + ph.p_vaddr) as *mut u8;
                let off = ph.p_offset as usize;
                let len = ph.p_filesz as usize;
                let src = interp_bytes
                    .get(off..off + len)
                    .ok_or_else(|| RunError::Platform("interp segment past EOF".into()))?;
                std::ptr::copy_nonoverlapping(src.as_ptr(), dst, len);
            }
        }
        // Patch SYSCALLs in interpreter as well.
        unsafe {
            use goblin::elf::program_header::PF_X;
            for ph in &interp_loads {
                if ph.p_flags & PF_X == 0 {
                    continue;
                }
                let base = (interp_bias + ph.p_vaddr) as *mut u8;
                let len = ph.p_filesz as usize;
                let seg = std::slice::from_raw_parts_mut(base, len);
                for i in 0..len.saturating_sub(1) {
                    if seg[i] == 0x0f && seg[i + 1] == 0x05 {
                        seg[i] = 0xcc;
                        seg[i + 1] = 0x90;
                        patched.insert(base as u64 + i as u64, ());
                    }
                }
            }
            // Also mirror interpreter into shadow space.
            use goblin::elf::program_header::{PF_R, PF_W};
            for ph in &interp_loads {
                let map_base = (interp_bias + ph.p_vaddr) & !(PAGE_SIZE - 1);
                let memsz = round_up((ph.p_vaddr % PAGE_SIZE) + ph.p_memsz, PAGE_SIZE);
                let prot = Protection {
                    read: ph.p_flags & PF_R != 0,
                    write: ph.p_flags & PF_W != 0,
                    execute: ph.p_flags & PF_X != 0,
                };
                let _ = space.map(map_base, memsz, prot, "interp");
                let data_ptr = (interp_bias + ph.p_vaddr) as *const u8;
                let data = std::slice::from_raw_parts(data_ptr, ph.p_filesz as usize);
                let _ = space.write_raw(interp_bias + ph.p_vaddr, data);
            }
        }
        interp_load_base = interp_min + interp_bias;
        jump_entry = interp_bias + interp_elf.header.e_entry;
    } else {
        interp_load_base = 0;
        jump_entry = entry;
    }

    let loaded_elf = anyos_elf::LoadedElf {
        entry,
        load_base: min_page + load_bias,
        phdr_vaddr,
        phdr_count: elf.header.e_phnum,
        phdr_entry_size: elf.header.e_phentsize,
        arch: ElfArch::X86_64,
        linkage: anyos_elf::Linkage::Static,
        is_pie,
    };
    let mut proc = LinuxProcess::new(space, loaded_elf, initial_brk);

    // ── build the stack in real memory ────────────────────────────────────
    const STACK_SIZE: u64 = 8 << 20;
    let stack_mem = unsafe {
        valloc(0, STACK_SIZE as usize, false)
            .ok_or_else(|| RunError::Platform("stack alloc failed".into()))?
    };
    let stack_top = stack_mem as u64 + STACK_SIZE;

    // Seed for AT_RANDOM — non-cryptographic, just needs to be non-zero.
    let at_random: [u8; 16] = {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos();
        let mut b = [0u8; 16];
        b[..4].copy_from_slice(&t.to_le_bytes());
        b
    };

    // Use a shadow AddressSpace to compute the stack layout.
    let mut stack_space = AddressSpace::new(
        stack_mem as u64,
        stack_top + PAGE_SIZE,
        STACK_SIZE + PAGE_SIZE,
    );
    let rsp = build_stack(
        &mut stack_space,
        &StackParams {
            stack_top,
            stack_size: STACK_SIZE,
            argv: opts.argv,
            envp: opts.envp,
            at_phdr: phdr_vaddr,
            at_phnum: elf.header.e_phnum as u64,
            at_phent: elf.header.e_phentsize as u64,
            at_entry: entry,
            at_base: interp_load_base,
            at_random,
        },
    )
    .map_err(RunError::Memory)?;

    // Copy computed stack bytes to real memory.
    unsafe {
        let len = (stack_top - rsp) as usize;
        let mut buf = vec![0u8; len];
        let _ = stack_space.read_raw(rsp, &mut buf);
        std::ptr::copy_nonoverlapping(buf.as_ptr(), rsp as *mut u8, len);
    }

    // ── install VEH and run ────────────────────────────────────────────────
    // The shadow space only holds the image so far; the dispatcher needs the
    // stack too (iov arrays, string arguments and locals all live there).
    proc.memory
        .map(
            stack_mem as u64,
            STACK_SIZE,
            Protection {
                read: true,
                write: true,
                execute: false,
            },
            "stack",
        )
        .map_err(RunError::Memory)?;

    // Real-memory regions mirrored into the shadow space, synced around each
    // syscall (real → shadow before, shadow → real after).
    let mut regions: Vec<(u64, u64)> = loads
        .iter()
        .map(|ph| (load_bias + ph.p_vaddr, ph.p_memsz))
        .collect();
    regions.push((stack_mem as u64, STACK_SIZE));

    {
        let mut guard = GUEST.lock().unwrap();
        *guard = Some(GuestState {
            patched,
            proc: Some(proc),
            exit_code: None,
            regions,
            stack: (stack_mem as u64, STACK_SIZE),
        });
    }

    let veh_handle = unsafe { AddVectoredExceptionHandler(1, veh) };
    if veh_handle.is_null() {
        return Err(RunError::Platform(
            "AddVectoredExceptionHandler failed".into(),
        ));
    }

    let raw_exit = unsafe { jump_to_entry(jump_entry, rsp) };

    unsafe { RemoveVectoredExceptionHandler(veh_handle) };

    // Prefer exit code set by exit/exit_group syscall.
    let code = {
        let guard = GUEST.lock().unwrap();
        if let Some(s) = guard.as_ref() {
            s.exit_code.unwrap_or(raw_exit)
        } else {
            raw_exit
        }
    };

    unsafe {
        VirtualFree((min_page + load_bias) as PVOID, 0, MEM_RELEASE);
        VirtualFree(stack_mem as PVOID, 0, MEM_RELEASE);
        if interp_load_base != 0 {
            VirtualFree(interp_load_base as PVOID, 0, MEM_RELEASE);
        }
    }

    Ok(RunResult { exit_code: code })
}

/// Set rsp to the guest stack pointer and jump to the entry point.
/// Returns when the guest's `main` returns (through `_start`'s call to `exit`).
#[cfg(target_os = "windows")]
unsafe fn jump_to_entry(entry: u64, rsp: u64) -> i32 {
    // We can't easily return from the guest (it will call exit via syscall),
    // so we wrap this in a setjmp/longjmp-style approach using a thread.
    // For simplicity: spin up a thread, join it, grab its "return code".
    // The guest will call exit_group which sets GUEST.exit_code, then we
    // catch the resulting SIGTRAP (INT3 after NOP) and return.
    //
    // A cleaner implementation would use fibers or structured exception
    // handling to unwind back here. For the first milestone we use a
    // dedicated OS thread so that the VEH fires on that thread.

    use std::sync::atomic::{AtomicI32, Ordering};
    static EXIT_CODE: AtomicI32 = AtomicI32::new(1);

    extern "system" fn guest_thread(arg: *mut core::ffi::c_void) -> u32 {
        let params = unsafe { Box::from_raw(arg as *mut (u64, u64)) };
        let (entry, rsp) = *params;
        unsafe { run_entry(entry, rsp) };
        0
    }

    extern "system" {
        fn CreateThread(
            attr: *mut core::ffi::c_void,
            stack: usize,
            func: extern "system" fn(*mut core::ffi::c_void) -> u32,
            arg: *mut core::ffi::c_void,
            flags: u32,
            id: *mut u32,
        ) -> *mut core::ffi::c_void;
        fn WaitForSingleObject(handle: *mut core::ffi::c_void, ms: u32) -> u32;
        fn CloseHandle(handle: *mut core::ffi::c_void) -> i32;
    }

    let params = Box::into_raw(Box::new((entry, rsp))) as *mut core::ffi::c_void;
    let h = CreateThread(
        std::ptr::null_mut(),
        0,
        guest_thread,
        params,
        0,
        std::ptr::null_mut(),
    );
    if h.is_null() {
        return 1;
    }
    const INFINITE: u32 = 0xFFFF_FFFF;
    WaitForSingleObject(h, INFINITE);
    CloseHandle(h);

    EXIT_CODE.load(Ordering::Relaxed)
}

#[cfg(target_os = "windows")]
#[unsafe(naked)]
unsafe extern "C" fn run_entry(entry: u64, rsp: u64) {
    // RCX = entry, RDX = rsp (Windows x64 calling convention).
    core::arch::naked_asm!("mov rsp, rdx", "xor rbp, rbp", "xor eax, eax", "jmp rcx",);
}

// ═══════════════════════════════════════════════════════════════════════════
// Linux — run the ELF directly in a child process (for CI / testing)
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(target_os = "linux")]
fn linux_direct(bytes: &[u8], opts: &RunOptions<'_>) -> Result<RunResult, RunError> {
    use std::io::Write;
    use std::process::Command;

    let dir = linux_tempdir()?;
    let exe = dir.join("guest.elf");
    {
        let mut f = std::fs::File::create(&exe)?;
        f.write_all(bytes)?;
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(std::fs::Permissions::from_mode(0o755))?;
    }

    let mut cmd = Command::new(&exe);
    if opts.argv.len() > 1 {
        cmd.args(&opts.argv[1..]);
    }
    for e in opts.envp {
        if let Some((k, v)) = e.split_once('=') {
            cmd.env(k, v);
        }
    }
    let status = cmd.status()?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(RunResult {
        exit_code: status.code().unwrap_or(1),
    })
}

#[cfg(target_os = "linux")]
fn linux_tempdir() -> Result<std::path::PathBuf, RunError> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!("anyos-{}-{seq}", std::process::id()));
    std::fs::create_dir_all(&p)?;
    Ok(p)
}
