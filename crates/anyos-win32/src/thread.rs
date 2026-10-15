//! Guest threads: start-up structures, guest calls, call-gate dispatch and a
//! cooperative scheduler.
//!
//! Every guest thread runs on the one execution backend. The scheduler lives
//! in the outermost run loop: it switches threads when a time slice runs out,
//! when a thread finishes, and when an API call has to wait. A waiting call
//! leaves the guest stopped on the call-gate stub, so when the thread is picked
//! again the same API call simply runs again and re-checks what it waits for.
//!
//! Calls made from inside an API implementation (a `DllMain` run by
//! `LoadLibrary`, an `InitOnce` callback) run nested on the calling thread
//! only, the way Windows runs them under the loader lock.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyos_backend::{x86_reg, ExecutionBackend, GuestContext, StopReason, X86Context};
use anyos_loader::Process;
use anyos_memory::{round_up, AddressSpace, MemoryError, Protection, ALLOCATION_GRANULARITY};
use anyos_pe::{ImportName, Machine};

use crate::api::{self, Api};
use crate::module::SystemModule;
use crate::objects::Kernel;
use crate::{RunError, RunOptions, Stdio};

pub(crate) const DLL_PROCESS_ATTACH: u64 = 1;
const DLL_THREAD_ATTACH: u64 = 2;
const DLL_THREAD_DETACH: u64 = 3;

/// Guest instructions a thread runs before another one gets a turn.
const TIME_SLICE: u64 = 200_000;

/// Why the guest stopped running before a call returned.
pub(crate) enum Halt {
    /// `ExitProcess` (or an equivalent) with this exit code.
    Exit(u32),
    /// `ExitThread` on the calling thread.
    ExitThread(u32),
    /// The API call has to wait: run other threads, then call it again.
    Block,
    /// Replace the entire CPU context (used by SwitchToFiber).
    SwitchContext(X86Context),
    Error(RunError),
    /// The API has already set `ctx` to the desired execution state (e.g.
    /// after a successful SEH `ExceptionContinueExecution`). The dispatcher
    /// must skip its normal `ret()` path and resume execution directly from
    /// `ctx.rip`.
    DirectResume,
}

impl From<RunError> for Halt {
    fn from(e: RunError) -> Self {
        Halt::Error(e)
    }
}

impl From<MemoryError> for Halt {
    fn from(e: MemoryError) -> Self {
        Halt::Error(RunError::Memory(e))
    }
}

/// Offsets of the TEB and PEB fields AnyOS fills in, per pointer size.
pub(crate) struct Layout {
    teb_exception_list: u64,
    teb_stack_base: u64,
    teb_stack_limit: u64,
    teb_self: u64,
    teb_process_id: u64,
    teb_thread_id: u64,
    teb_tls_pointer: u64,
    teb_peb: u64,
    teb_last_error: u64,
    /// The 64 dynamic TLS slots (`TlsAlloc`).
    pub teb_tls_slots: u64,
    /// Pointer to 1024 more dynamic TLS slots, allocated on first use.
    pub teb_tls_expansion: u64,
    peb_image_base: u64,
    peb_process_heap: u64,
}

const LAYOUT_64: Layout = Layout {
    teb_exception_list: 0x00,
    teb_stack_base: 0x08,
    teb_stack_limit: 0x10,
    teb_self: 0x30,
    teb_process_id: 0x40,
    teb_thread_id: 0x48,
    teb_tls_pointer: 0x58,
    teb_peb: 0x60,
    teb_last_error: 0x68,
    teb_tls_slots: 0x1480,
    teb_tls_expansion: 0x1780,
    peb_image_base: 0x10,
    peb_process_heap: 0x30,
};

const LAYOUT_32: Layout = Layout {
    teb_exception_list: 0x00,
    teb_stack_base: 0x04,
    teb_stack_limit: 0x08,
    teb_self: 0x18,
    teb_process_id: 0x20,
    teb_thread_id: 0x24,
    teb_tls_pointer: 0x2C,
    teb_peb: 0x30,
    teb_last_error: 0x34,
    teb_tls_slots: 0xE10,
    teb_tls_expansion: 0xF94,
    peb_image_base: 0x08,
    peb_process_heap: 0x18,
};

pub(crate) const PROCESS_ID: u64 = 0x1000;
pub(crate) const MAIN_THREAD_ID: u32 = 0x1004;

/// One guest thread. The running thread's context, TEB and stack top live in
/// [`Thread`] itself; the copies here are stale until it is switched out.
pub(crate) struct GuestThread {
    pub id: u32,
    pub ctx: X86Context,
    pub teb: u64,
    pub stack_top: u64,
    /// Base of the stack mapping, released when the thread ends.
    pub stack_base: u64,
    pub exit_code: Option<u32>,
    pub suspend: u32,
    /// Whether `DLL_THREAD_ATTACH` has run and the start routine is entered.
    pub started: bool,
    /// The last API call it made is waiting; skip it until something changes
    /// or `wake_at` passes.
    pub blocked: bool,
    pub wake_at: Option<Instant>,
    /// When the API call it is waiting in started waiting.
    pub wait_started: Option<Instant>,
    pub priority: i32,
    pub fls: Vec<u64>,
    pub cv: Option<crate::sync::CvWait>,
    pub description: String,
}

impl GuestThread {
    fn new(id: u32, ctx: X86Context, teb: u64, stack_top: u64, stack_base: u64) -> Self {
        GuestThread {
            id,
            ctx,
            teb,
            stack_top,
            stack_base,
            exit_code: None,
            suspend: 0,
            started: false,
            blocked: false,
            wake_at: None,
            wait_started: None,
            priority: 0,
            fls: Vec::new(),
            cv: None,
            description: String::new(),
        }
    }

    pub fn alive(&self) -> bool {
        self.exit_code.is_none()
    }
}

/// The guest-visible host configuration: where `C:` lives and the registry.
#[derive(Debug, Clone, Default)]
pub(crate) struct HostConfig {
    pub drive_c: Option<PathBuf>,
    pub registry_file: Option<PathBuf>,
    pub user: String,
    pub child_runner: Vec<std::ffi::OsString>,
}

pub(crate) struct Thread<'a, 'b> {
    pub process: Process,
    pub ctx: X86Context,
    pub backend: &'b mut dyn ExecutionBackend,
    pub stdio: Stdio<'a>,
    pub trace: bool,
    pub limit: Option<u64>,
    pub api_calls: u64,
    /// Return address that hands control back to the runtime.
    sentinel: u64,
    pub stack_top: u64,
    pub teb: u64,
    pub peb: u64,
    pub cmdline_a: u64,
    pub cmdline_w: u64,
    pub layout: &'static Layout,
    /// How many API implementations are running; a guest call made from one
    /// (a DllMain run by LoadLibrary) must not reuse the stack from the top.
    api_depth: u32,
    /// Run loops started from inside API implementations. Only the outermost
    /// loop switches threads.
    nested_loops: u32,
    /// Where LoadLibrary looks for application DLLs.
    pub search_dirs: Vec<PathBuf>,
    /// Handles given out for system DLLs, which AnyOS provides itself.
    pub system_modules: Vec<SystemModule>,
    pub error_mode: u32,
    pub threads: Vec<GuestThread>,
    pub current: usize,
    /// The running thread asked to give up the rest of its time slice.
    pub yield_now: bool,
    pub next_thread_id: u32,
    pub k: Kernel,
    pub host: HostConfig,
    pub started_at: Instant,
}

/// Map a stack, a TEB and the static TLS blocks for a new thread. Returns
/// `(stack base, stack size, teb)`.
fn new_thread_memory(
    process: &mut Process,
    layout: &Layout,
    stack_size: u64,
    tid: u32,
    peb: u64,
) -> Result<(u64, u64, u64), MemoryError> {
    let long = process.machine == Machine::X64;
    let ptr = if long { 8 } else { 4 };
    let stack_size = round_up(
        stack_size.clamp(0x4_0000, 0x100_0000),
        ALLOCATION_GRANULARITY,
    );
    let tls_array = static_tls(process, ptr)?;
    let mem = &mut process.memory;
    let stack = mem.map_anywhere(stack_size, Protection::RW, "thread stack", None)?;
    let teb = mem.map_anywhere(0x2000, Protection::RW, "TEB", None)?;
    let l = layout;
    let p = ptr as usize;
    mem.write_ptr_raw(
        teb + l.teb_exception_list,
        p,
        if long { 0 } else { u64::MAX },
    )?;
    mem.write_ptr_raw(teb + l.teb_stack_base, p, stack + stack_size)?;
    mem.write_ptr_raw(teb + l.teb_stack_limit, p, stack)?;
    mem.write_ptr_raw(teb + l.teb_self, p, teb)?;
    mem.write_ptr_raw(teb + l.teb_process_id, p, PROCESS_ID)?;
    mem.write_ptr_raw(teb + l.teb_thread_id, p, tid as u64)?;
    mem.write_ptr_raw(teb + l.teb_tls_pointer, p, tls_array)?;
    mem.write_ptr_raw(teb + l.teb_peb, p, peb)?;
    Ok((stack, stack_size, teb))
}

/// Static TLS: one fresh block per module with a TLS directory, indexed by slot.
fn static_tls(process: &mut Process, ptr: u64) -> Result<u64, MemoryError> {
    let slots: Vec<_> = process
        .modules
        .iter()
        .filter_map(|m| m.tls.clone())
        .collect();
    if slots.is_empty() {
        return Ok(0);
    }
    let mem: &mut AddressSpace = &mut process.memory;
    let count = slots.iter().map(|t| t.index as u64 + 1).max().unwrap_or(0);
    let tls_array = mem.map_anywhere(count * ptr, Protection::RW, "TLS slots", None)?;
    for t in &slots {
        let template = t.template_end.saturating_sub(t.template_start);
        let size = (template + t.zero_fill as u64).max(1);
        let block = mem.map_anywhere(size, Protection::RW, "TLS data", None)?;
        if template > 0 {
            let mut data = vec![0u8; template as usize];
            mem.read_raw(t.template_start, &mut data)?;
            mem.write_raw(block, &data)?;
        }
        mem.write_ptr_raw(tls_array + t.index as u64 * ptr, ptr as usize, block)?;
    }
    Ok(tls_array)
}

impl<'a, 'b> Thread<'a, 'b> {
    pub fn new(
        mut process: Process,
        cmdline: &str,
        opts: &RunOptions,
        backend: &'b mut dyn ExecutionBackend,
        stdio: Stdio<'a>,
    ) -> Result<Self, RunError> {
        let long = process.machine == Machine::X64;
        let layout = if long { &LAYOUT_64 } else { &LAYOUT_32 };
        let main_base = process.modules[0].base;
        let stack_reserve = process.modules[0].stack_reserve;

        let peb = process
            .memory
            .map_anywhere(0x1000, Protection::RW, "PEB", None)?;
        let (stack, stack_size, teb) =
            new_thread_memory(&mut process, layout, stack_reserve, MAIN_THREAD_ID, peb)?;

        // Command line, ANSI then UTF-16, in one read-only block.
        let mem = &mut process.memory;
        let ansi: Vec<u8> = cmdline.bytes().chain([0]).collect();
        let wide: Vec<u8> = cmdline
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        let wide_off = round_up(ansi.len() as u64, 16);
        let params = mem.map_anywhere(
            wide_off + wide.len() as u64,
            Protection::RW,
            "process parameters",
            None,
        )?;
        mem.write_raw(params, &ansi)?;
        mem.write_raw(params + wide_off, &wide)?;
        mem.protect(
            params,
            round_up(wide_off + wide.len() as u64, 0x1000),
            Protection::R,
        )?;
        let p = if long { 8 } else { 4 };
        mem.write_ptr_raw(peb + layout.peb_image_base, p, main_base)?;

        let sentinel = process.call_gate.stub_for(
            &mut process.memory,
            process.machine,
            "anyos",
            &ImportName::Name {
                name: "ReturnToHost".into(),
                hint: 0,
            },
        )?;

        let program_dir = process.modules[0]
            .path
            .as_ref()
            .and_then(|p| p.parent())
            .map(|d| {
                if d.as_os_str().is_empty() {
                    PathBuf::from(".")
                } else {
                    d.to_path_buf()
                }
            });
        let mut search_dirs: Vec<PathBuf> = program_dir.clone().into_iter().collect();
        search_dirs.extend(opts.search_dirs.iter().cloned());

        let mut ctx = X86Context::new(long);
        if long {
            ctx.gs_base = teb;
        } else {
            ctx.fs_base = teb;
        }
        // Leave a little room above the first frame, as Windows does.
        let stack_top = stack + stack_size - 0x100;
        let mut main = GuestThread::new(MAIN_THREAD_ID, ctx.clone(), teb, stack_top, stack);
        main.started = true;

        let host = HostConfig {
            drive_c: opts.drive_c.clone(),
            registry_file: opts.registry_file.clone(),
            user: opts.user.clone().unwrap_or_else(default_user),
            child_runner: opts.child_runner.clone(),
        };
        let mut t = Thread {
            process,
            ctx,
            backend,
            stdio,
            trace: opts.trace,
            limit: opts.instruction_limit,
            api_calls: 0,
            sentinel,
            stack_top,
            teb,
            peb,
            cmdline_a: params,
            cmdline_w: params + wide_off,
            layout,
            api_depth: 0,
            nested_loops: 0,
            search_dirs,
            system_modules: Vec::new(),
            error_mode: 0,
            threads: vec![main],
            current: 0,
            yield_now: false,
            next_thread_id: MAIN_THREAD_ID + 4,
            k: Kernel::default(),
            host,
            started_at: Instant::now(),
        };
        let heap = crate::memory::create_heap(&mut t).map_err(|h| match h {
            Halt::Error(e) => e,
            _ => RunError::Memory(MemoryError::Exhausted { size: 0 }),
        })?;
        t.k.process_heap = heap;
        t.process
            .memory
            .write_ptr_raw(peb + layout.peb_process_heap, p, heap)?;
        crate::file::init(&mut t, program_dir.as_deref(), &opts.env);
        crate::registry::init(&mut t);
        Ok(t)
    }

    pub fn long_mode(&self) -> bool {
        self.ctx.long_mode
    }

    pub fn ptr_size(&self) -> usize {
        self.ctx.pointer_size()
    }

    pub fn thread_id(&self) -> u32 {
        self.threads[self.current].id
    }

    /// Initialise DLLs (TLS callbacks, then the entry point), then the
    /// program's TLS callbacks and entry point. Returns the exit code.
    pub fn start(&mut self) -> Result<u32, RunError> {
        let r = match self.start_inner() {
            Ok(code) | Err(Halt::Exit(code)) | Err(Halt::ExitThread(code)) => Ok(code),
            Err(Halt::Block) => Err(RunError::Deadlock {
                detail: "a wait could not be resumed".into(),
            }),
            Err(Halt::SwitchContext(_)) => Err(RunError::Deadlock {
                detail: "SwitchToFiber used without a fiber context".into(),
            }),
            Err(Halt::Error(e)) => Err(e),
            // DirectResume is handled inside call_from; it should not bubble here.
            Err(Halt::DirectResume) => Err(RunError::Deadlock {
                detail: "DirectResume reached the top-level run loop unexpectedly".into(),
            }),
        };
        crate::file::shutdown(self);
        crate::registry::shutdown(self);
        r
    }

    fn start_inner(&mut self) -> Result<u32, Halt> {
        // Dependencies were appended after the modules that import them, so
        // initialise in reverse order: deepest dependency first.
        for i in (1..self.process.modules.len()).rev() {
            if !self.init_dll(i)? {
                let module = self.process.modules[i].name.clone();
                return Err(RunError::DllInitFailed { module }.into());
            }
        }
        self.run_tls_callbacks(0, DLL_PROCESS_ATTACH)?;
        let Some(entry) = self.process.modules[0].entry_point else {
            return Ok(0);
        };
        // Returning from the entry point ends the process, with the returned
        // value as the exit code.
        let ret = self.call(entry, &[])?;
        Ok(ret as u32)
    }

    /// Run a DLL's TLS callbacks and entry point for `DLL_PROCESS_ATTACH`.
    /// Returns what the entry point returned (TRUE when it has none).
    pub fn init_dll(&mut self, module: usize) -> Result<bool, Halt> {
        let m = &self.process.modules[module];
        let (base, entry) = (m.base, m.entry_point);
        self.run_tls_callbacks(module, DLL_PROCESS_ATTACH)?;
        let Some(entry) = entry else {
            return Ok(true);
        };
        let ok = self.call(entry, &[base, DLL_PROCESS_ATTACH, 0])?;
        Ok(ok & 0xFFFF_FFFF != 0)
    }

    fn run_tls_callbacks(&mut self, module: usize, reason: u64) -> Result<(), Halt> {
        let m = &self.process.modules[module];
        let base = m.base;
        let callbacks = m
            .tls
            .as_ref()
            .map(|t| t.callbacks.clone())
            .unwrap_or_default();
        for cb in callbacks.into_iter().take_while(|&cb| cb != 0) {
            self.call(cb, &[base, reason, 0])?;
        }
        Ok(())
    }

    /// Tell every DLL (and the program's TLS callbacks) that the running
    /// thread starts or ends, as nested calls on that thread.
    fn notify_dlls(&mut self, reason: u64) -> Result<(), Halt> {
        self.api_depth += 1;
        let r = (|| {
            for i in (0..self.process.modules.len()).rev() {
                self.run_tls_callbacks(i, reason)?;
                let m = &self.process.modules[i];
                if !m.image.is_dll {
                    continue;
                }
                if let Some(entry) = m.entry_point {
                    let base = m.base;
                    self.call(entry, &[base, reason, 0])?;
                }
            }
            Ok(())
        })();
        self.api_depth -= 1;
        r
    }

    /// Call guest code at `target` with the platform calling convention
    /// (Microsoft x64, or stdcall on x86) and run until it returns.
    ///
    /// From inside an API implementation the guest is paused mid-call, so the
    /// new frame goes below its stack pointer and its registers are restored
    /// afterwards.
    pub fn call(&mut self, target: u64, args: &[u64]) -> Result<u64, Halt> {
        if self.api_depth == 0 {
            return self.call_from(target, args);
        }
        let saved = self.ctx.clone();
        let saved_top = self.stack_top;
        self.stack_top = saved.regs[x86_reg::RSP].saturating_sub(0x100);
        self.nested_loops += 1;
        let result = self.call_from(target, args);
        self.nested_loops -= 1;
        self.stack_top = saved_top;
        self.ctx = saved;
        result
    }

    /// Set up `ctx` to call `target` with `args` on a stack whose top is
    /// `stack_top`, returning to the sentinel.
    pub(crate) fn push_frame(
        &mut self,
        ctx_is_current: bool,
        stack_top: u64,
        target: u64,
        args: &[u64],
    ) -> Result<X86Context, MemoryError> {
        let mut ctx = if ctx_is_current {
            self.ctx.clone()
        } else {
            let mut c = X86Context::new(self.ctx.long_mode);
            c.fs_base = self.ctx.fs_base;
            c.gs_base = self.ctx.gs_base;
            c
        };
        let p = self.ptr_size();
        let mem = &mut self.process.memory;
        let mut sp = stack_top & !0xF;
        if ctx.long_mode {
            let slots = round_up(args.len().max(4) as u64 * 8, 16);
            sp -= slots;
            for (n, &a) in args.iter().enumerate() {
                match n {
                    0 => ctx.regs[x86_reg::RCX] = a,
                    1 => ctx.regs[x86_reg::RDX] = a,
                    2 => ctx.regs[x86_reg::R8] = a,
                    3 => ctx.regs[x86_reg::R9] = a,
                    _ => {}
                }
                mem.write_ptr_raw(sp + n as u64 * 8, 8, a)?;
            }
        } else {
            for &a in args.iter().rev() {
                sp -= 4;
                mem.write_ptr_raw(sp, 4, a)?;
            }
        }
        sp -= p as u64;
        mem.write_ptr_raw(sp, p, self.sentinel)?;
        ctx.regs[x86_reg::RSP] = sp;
        ctx.rip = target;
        ctx.rflags = X86Context::INITIAL_FLAGS;
        Ok(ctx)
    }

    fn call_from(&mut self, target: u64, args: &[u64]) -> Result<u64, Halt> {
        self.ctx = self.push_frame(true, self.stack_top, target, args)?;
        let me = self.current;
        let scheduler = self.nested_loops == 0;

        loop {
            let slice = scheduler && self.others_runnable();
            let budget = match (self.remaining(), slice) {
                (Some(r), true) => Some(r.min(TIME_SLICE)),
                (Some(r), false) => Some(r),
                (None, true) => Some(TIME_SLICE),
                (None, false) => None,
            };
            let mut gc =
                GuestContext::X86(std::mem::replace(&mut self.ctx, X86Context::new(false)));
            let stop = self.backend.run(&mut gc, &mut self.process.memory, budget);
            #[allow(unreachable_patterns)] // GuestContext is non_exhaustive.
            match gc {
                GuestContext::X86(c) => self.ctx = c,
                _ => unreachable!("the backend changed the guest architecture"),
            }
            match stop {
                StopReason::Breakpoint { address } if address == self.sentinel => {
                    let rax = self.ctx.regs[x86_reg::RAX];
                    if self.current == me {
                        return Ok(rax);
                    }
                    // A thread's start routine returned: that ends the thread.
                    if let Some(code) = self.end_thread(rax as u32)? {
                        return Ok(code as u64);
                    }
                }
                StopReason::Breakpoint { address } => {
                    let Some(entry) = self.process.call_gate.lookup(address).cloned() else {
                        return Err(RunError::Breakpoint {
                            location: self.describe(address),
                        }
                        .into());
                    };
                    match self.dispatch(&entry.dll, &entry.symbol) {
                        Ok(()) | Err(Halt::DirectResume) => {
                            if self.yield_now && scheduler {
                                self.yield_now = false;
                                self.schedule()?;
                            }
                        }
                        Err(Halt::Block) if scheduler => {
                            // Resume on the stub itself, so the call runs again.
                            self.ctx.rip = address;
                            self.threads[self.current].blocked = true;
                            self.schedule()?;
                        }
                        Err(Halt::Block) => {
                            return Err(RunError::Deadlock {
                                detail: format!(
                                    "{}!{} waits for another thread while a DLL entry point or callback runs",
                                    entry.dll, entry.symbol
                                ),
                            }
                            .into());
                        }
                        Err(Halt::ExitThread(code)) if scheduler => {
                            if let Some(code) = self.end_thread(code)? {
                                return Ok(code as u64);
                            }
                        }
                        Err(e) => return Err(e),
                    }
                }
                StopReason::Fault(fault) => {
                    let location = self.describe(fault.pc());
                    return Err(RunError::Fault { fault, location }.into());
                }
                StopReason::InstructionLimit => {
                    if self.remaining() == Some(0) {
                        return Err(RunError::InstructionLimit(self.limit.unwrap_or(0)).into());
                    }
                    self.schedule()?;
                }
            }
        }
    }

    fn remaining(&self) -> Option<u64> {
        self.limit
            .map(|l| l.saturating_sub(self.backend.instructions_executed()))
    }

    fn runnable(&self, i: usize, now: Instant) -> bool {
        let th = &self.threads[i];
        th.alive() && th.suspend == 0 && (!th.blocked || th.wake_at.is_some_and(|w| w <= now))
    }

    fn others_runnable(&self) -> bool {
        self.threads
            .iter()
            .enumerate()
            .any(|(i, t)| i != self.current && t.alive() && t.suspend == 0)
    }

    /// Switch to the next thread that can run, sleeping until a timed wait
    /// expires when every thread waits.
    pub fn schedule(&mut self) -> Result<(), Halt> {
        loop {
            let now = Instant::now();
            let n = self.threads.len();
            let next = (1..=n)
                .map(|k| (self.current + k) % n)
                .find(|&i| self.runnable(i, now));
            if let Some(i) = next {
                self.threads[i].blocked = false;
                self.threads[i].wake_at = None;
                return self.switch_to(i);
            }
            let earliest = self
                .threads
                .iter()
                .filter(|t| t.alive() && t.suspend == 0 && t.blocked)
                .filter_map(|t| t.wake_at)
                .min();
            match earliest {
                Some(at) => std::thread::sleep(at.saturating_duration_since(now)),
                None => {
                    let waiting = self.threads.iter().filter(|t| t.alive()).count();
                    return Err(RunError::Deadlock {
                        detail: format!(
                            "{waiting} thread(s) wait for each other and nothing can wake them"
                        ),
                    }
                    .into());
                }
            }
        }
    }

    /// Make thread `i` the running one, entering its start routine (after
    /// `DLL_THREAD_ATTACH`) the first time.
    fn switch_to(&mut self, i: usize) -> Result<(), Halt> {
        if i != self.current {
            let cur = self.current;
            let ctx = std::mem::replace(&mut self.ctx, self.threads[i].ctx.clone());
            let th = &mut self.threads[cur];
            th.ctx = ctx;
            th.teb = self.teb;
            th.stack_top = self.stack_top;
            self.teb = self.threads[i].teb;
            self.stack_top = self.threads[i].stack_top;
            self.current = i;
        }
        if !self.threads[i].started {
            self.threads[i].started = true;
            self.notify_dlls(DLL_THREAD_ATTACH)?;
        }
        Ok(())
    }

    /// Wake every waiting thread so it re-checks what it waits for.
    pub fn wake_all(&mut self) {
        for t in &mut self.threads {
            t.blocked = false;
            t.wake_at = None;
        }
    }

    /// Create a thread that will start at `start(param)`.
    pub fn create_thread(
        &mut self,
        start: u64,
        param: u64,
        stack_size: u64,
        suspended: bool,
    ) -> Result<usize, Halt> {
        let tid = self.next_thread_id;
        self.next_thread_id += 4;
        let size = if stack_size == 0 {
            self.process.modules[0].stack_reserve
        } else {
            stack_size
        };
        let (stack, stack_size, teb) =
            new_thread_memory(&mut self.process, self.layout, size, tid, self.peb)?;
        let top = stack + stack_size - 0x100;
        let mut ctx = self.push_frame(false, top, start, &[param])?;
        if ctx.long_mode {
            ctx.gs_base = teb;
            ctx.fs_base = 0;
        } else {
            ctx.fs_base = teb;
            ctx.gs_base = 0;
        }
        let mut th = GuestThread::new(tid, ctx, teb, top, stack);
        th.suspend = u32::from(suspended);
        self.threads.push(th);
        Ok(self.threads.len() - 1)
    }

    /// The running thread ends with `code`. Returns the process exit code
    /// when no thread is left, otherwise switches to another thread.
    fn end_thread(&mut self, code: u32) -> Result<Option<u32>, Halt> {
        if self.threads[self.current].alive() {
            crate::process::run_fls_callbacks(self)?;
            self.notify_dlls(DLL_THREAD_DETACH)?;
            self.threads[self.current].exit_code = Some(code);
            crate::sync::thread_ended(self);
            self.wake_all();
        }
        self.end_thread_switch()
    }

    fn end_thread_switch(&mut self) -> Result<Option<u32>, Halt> {
        let code = self.threads[self.current].exit_code.unwrap_or(0);
        if !self.threads.iter().any(GuestThread::alive) {
            return Ok(Some(code));
        }
        self.release_thread_memory(self.current);
        self.schedule()?;
        Ok(None)
    }

    /// Free a finished thread's stack (never the main thread's, whose frames
    /// the outermost call still owns).
    pub fn release_thread_memory(&mut self, i: usize) {
        if i == 0 || self.threads[i].stack_base == 0 {
            return;
        }
        let _ = self.process.memory.unmap(self.threads[i].stack_base);
        self.threads[i].stack_base = 0;
    }

    /// Whether a waiting API may hand the CPU to other threads (only from the
    /// outermost run loop).
    pub fn can_switch(&self) -> bool {
        self.nested_loops == 0
    }

    /// Called by an API that has to wait until `deadline` (`None`: forever).
    /// Returns `Ok(true)` once the deadline has passed, `Ok(false)` when the
    /// caller should check again right away, and [`Halt::Block`] to run other
    /// threads first. `poll` asks to be checked again soon even with no
    /// deadline (for things that change outside the guest, like a child
    /// process ending).
    pub fn wait_or_block(&mut self, deadline: Option<Instant>, poll: bool) -> Result<bool, Halt> {
        let now = Instant::now();
        if deadline.is_some_and(|d| d <= now) {
            return Ok(true);
        }
        let poll_at = poll.then(|| now + Duration::from_millis(5));
        let wake = match (deadline, poll_at) {
            (Some(d), Some(p)) => Some(d.min(p)),
            (d, p) => d.or(p),
        };
        if self.can_switch() {
            let th = &mut self.threads[self.current];
            th.wake_at = wake;
            return Err(Halt::Block);
        }
        match wake {
            Some(w) => {
                std::thread::sleep(w.saturating_duration_since(now));
                Ok(false)
            }
            None => Err(RunError::Deadlock {
                detail: "a wait with no timeout inside a DLL entry point or callback".into(),
            }
            .into()),
        }
    }

    /// When the API call the running thread is in started waiting.
    pub fn wait_start(&mut self) -> Instant {
        *self.threads[self.current]
            .wait_started
            .get_or_insert_with(Instant::now)
    }

    /// The deadline of a wait of `ms` milliseconds (`INFINITE` waits forever).
    pub fn deadline(&mut self, ms: u32) -> Option<Instant> {
        let start = self.wait_start();
        (ms != u32::MAX).then(|| start + Duration::from_millis(ms as u64))
    }

    /// Run the implementation of `dll!symbol` and return to the guest caller.
    fn dispatch(&mut self, dll: &str, symbol: &ImportName) -> Result<(), Halt> {
        self.api_calls += 1;
        let p = self.ptr_size();
        let ret_addr = self
            .process
            .memory
            .read_ptr_raw(self.ctx.regs[x86_reg::RSP], p)?;
        let Some(api) = api::find(dll, symbol) else {
            return Err(RunError::UnimplementedApi {
                dll: dll.to_string(),
                symbol: symbol.to_string(),
                caller: self.describe(ret_addr),
            }
            .into());
        };
        let count = if self.ctx.long_mode {
            api.args
        } else {
            api.args32
        };
        let args: Vec<u64> = (0..count).map(|n| self.arg(n)).collect::<Result<_, _>>()?;
        self.api_depth += 1;
        let result = (api.f)(self, &args);
        self.api_depth -= 1;
        if matches!(result, Err(Halt::Block)) {
            // Called again when the thread runs next; trace only the final result.
            self.api_calls -= 1;
            return Err(Halt::Block);
        }
        if matches!(result, Err(Halt::DirectResume)) {
            // The API restored ctx directly (e.g. SEH ExceptionContinueExecution).
            // Skip ret() entirely; execution resumes from ctx.rip.
            if self.trace {
                let shown: Vec<String> = args.iter().map(|a| format!("{a:#x}")).collect();
                let _ = writeln!(
                    self.stdio.stderr,
                    "[anyos] {}!{}({}) -> resume@{:#x}",
                    dll,
                    api.name,
                    shown.join(", "),
                    self.ctx.rip,
                );
            }
            return Err(Halt::DirectResume);
        }
        self.threads[self.current].wait_started = None;
        if self.trace {
            let shown: Vec<String> = args.iter().map(|a| format!("{a:#x}")).collect();
            let outcome = match &result {
                Ok(v) => format!("{v:#x}"),
                Err(Halt::Exit(code)) => format!("exit {code}"),
                Err(Halt::ExitThread(code)) => format!("thread exit {code}"),
                Err(Halt::Block) => "wait".into(),
                Err(Halt::SwitchContext(_)) => "switch".into(),
                Err(Halt::Error(_)) | Err(Halt::DirectResume) => "error".into(),
            };
            let _ = writeln!(
                self.stdio.stderr,
                "[anyos] {}!{}({}) -> {}",
                dll,
                api.name,
                shown.join(", "),
                outcome
            );
        }
        if let Err(Halt::SwitchContext(ctx)) = result {
            self.ctx = ctx;
            return Ok(());
        }
        let value = result?;
        self.ret(api, value)
    }

    /// Argument `n` of the API call the guest is making right now. Varargs
    /// APIs read past their fixed arguments with this.
    pub fn arg(&self, n: u8) -> Result<u64, MemoryError> {
        let sp = self.ctx.regs[x86_reg::RSP];
        let mem = &self.process.memory;
        if self.ctx.long_mode {
            Ok(match n {
                0 => self.ctx.regs[x86_reg::RCX],
                1 => self.ctx.regs[x86_reg::RDX],
                2 => self.ctx.regs[x86_reg::R8],
                3 => self.ctx.regs[x86_reg::R9],
                // Above the return address and the 32-byte home area.
                _ => mem.read_u64_raw(sp + 8 + n as u64 * 8)?,
            })
        } else {
            mem.read_u32_raw(sp + 4 + n as u64 * 4).map(u64::from)
        }
    }

    /// Return `value` to the caller: pop the return address and, for stdcall,
    /// the arguments. On x86 a 64-bit result goes in EDX:EAX.
    fn ret(&mut self, api: &Api, value: u64) -> Result<(), Halt> {
        let p = self.ptr_size() as u64;
        let sp = self.ctx.regs[x86_reg::RSP];
        let ret_addr = self.process.memory.read_ptr_raw(sp, p as usize)?;
        let mut new_sp = sp + p;
        if !self.ctx.long_mode {
            if !api.cdecl {
                new_sp += api.args32 as u64 * 4;
            }
            self.ctx.regs[x86_reg::RAX] = value & 0xFFFF_FFFF;
            self.ctx.regs[x86_reg::RDX] = value >> 32;
        } else {
            self.ctx.regs[x86_reg::RAX] = value;
        }
        self.ctx.regs[x86_reg::RSP] = new_sp;
        self.ctx.rip = ret_addr;
        Ok(())
    }

    /// With `--trace`, explain something the API results alone do not show.
    pub fn trace_note(&mut self, note: &str) {
        if self.trace {
            let _ = writeln!(self.stdio.stderr, "[anyos] {note}");
        }
    }

    pub fn last_error(&self) -> u32 {
        self.process
            .memory
            .read_u32_raw(self.teb + self.layout.teb_last_error)
            .unwrap_or(0)
    }

    pub fn set_last_error(&mut self, code: u32) {
        let at = self.teb + self.layout.teb_last_error;
        let _ = self.process.memory.write_raw(at, &code.to_le_bytes());
    }

    /// `module+0x1234`, `the call gate` or a raw address.
    pub fn describe(&self, addr: u64) -> String {
        if let Some(e) = self.process.call_gate.lookup(addr) {
            return format!("the call gate ({}!{})", e.dll, e.symbol);
        }
        for m in &self.process.modules {
            if addr >= m.base && addr < m.base + m.size {
                return format!("{}+{:#x}", m.name, addr - m.base);
            }
        }
        format!("{addr:#x}")
    }
}

fn default_user() -> String {
    ["USER", "USERNAME"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|u| {
            !u.is_empty()
                && u.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        })
        .unwrap_or_else(|| "user".into())
}
