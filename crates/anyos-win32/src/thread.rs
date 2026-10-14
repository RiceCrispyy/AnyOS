//! The main thread: start-up structures, guest calls and call-gate dispatch.

use std::path::PathBuf;

use anyos_backend::{x86_reg, ExecutionBackend, GuestContext, StopReason, X86Context};
use anyos_loader::Process;
use anyos_memory::{round_up, MemoryError, Protection, ALLOCATION_GRANULARITY};
use anyos_pe::{ImportName, Machine};

use crate::api::{self, Api};
use crate::module::SystemModule;
use crate::{RunError, RunOptions, Stdio};

const DLL_PROCESS_ATTACH: u64 = 1;

/// Why the guest stopped running before a call returned.
pub(crate) enum Halt {
    /// `ExitProcess` (or an equivalent) with this exit code.
    Exit(u32),
    Error(RunError),
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
struct Layout {
    teb_exception_list: u64,
    teb_stack_base: u64,
    teb_stack_limit: u64,
    teb_self: u64,
    teb_process_id: u64,
    teb_thread_id: u64,
    teb_tls_pointer: u64,
    teb_peb: u64,
    teb_last_error: u64,
    peb_image_base: u64,
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
    peb_image_base: 0x10,
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
    peb_image_base: 0x08,
};

pub(crate) const PROCESS_ID: u64 = 0x1000;
pub(crate) const THREAD_ID: u64 = 0x1004;

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
    stack_top: u64,
    pub teb: u64,
    pub cmdline_a: u64,
    pub cmdline_w: u64,
    layout: &'static Layout,
    /// How many API implementations are running; a guest call made from one
    /// (a DllMain run by LoadLibrary) must not reuse the stack from the top.
    api_depth: u32,
    /// Where LoadLibrary looks for application DLLs.
    pub search_dirs: Vec<PathBuf>,
    /// Handles given out for system DLLs, which AnyOS provides itself.
    pub system_modules: Vec<SystemModule>,
    pub error_mode: u32,
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
        let ptr = if long { 8 } else { 4 };
        let layout = if long { &LAYOUT_64 } else { &LAYOUT_32 };
        let main_base = process.modules[0].base;
        let stack_size = round_up(
            process.modules[0].stack_reserve.clamp(0x4_0000, 0x100_0000),
            ALLOCATION_GRANULARITY,
        );

        let mem = &mut process.memory;
        let stack = mem.map_anywhere(stack_size, Protection::RW, "main thread stack", None)?;
        let teb = mem.map_anywhere(0x2000, Protection::RW, "TEB", None)?;
        let peb = mem.map_anywhere(0x1000, Protection::RW, "PEB", None)?;

        // Command line, ANSI then UTF-16, in one read-only block.
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

        // Static TLS: one block per module with a TLS directory, indexed by slot.
        let slots: Vec<_> = process
            .modules
            .iter()
            .filter_map(|m| m.tls.clone())
            .collect();
        let mut tls_array = 0;
        if !slots.is_empty() {
            let count = slots.iter().map(|t| t.index as u64 + 1).max().unwrap_or(0);
            tls_array = mem.map_anywhere(count * ptr, Protection::RW, "TLS slots", None)?;
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
        }

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
        mem.write_ptr_raw(teb + l.teb_thread_id, p, THREAD_ID)?;
        mem.write_ptr_raw(teb + l.teb_tls_pointer, p, tls_array)?;
        mem.write_ptr_raw(teb + l.teb_peb, p, peb)?;
        mem.write_ptr_raw(peb + l.peb_image_base, p, main_base)?;

        let sentinel = process.call_gate.stub_for(
            &mut process.memory,
            process.machine,
            "anyos",
            &ImportName::Name {
                name: "ReturnToHost".into(),
                hint: 0,
            },
        )?;

        let mut search_dirs: Vec<PathBuf> = process.modules[0]
            .path
            .as_ref()
            .and_then(|p| p.parent())
            .map(|d| {
                if d.as_os_str().is_empty() {
                    PathBuf::from(".")
                } else {
                    d.to_path_buf()
                }
            })
            .into_iter()
            .collect();
        search_dirs.extend(opts.search_dirs.iter().cloned());

        let mut ctx = X86Context::new(long);
        if long {
            ctx.gs_base = teb;
        } else {
            ctx.fs_base = teb;
        }
        Ok(Thread {
            process,
            ctx,
            backend,
            stdio,
            trace: opts.trace,
            limit: opts.instruction_limit,
            api_calls: 0,
            sentinel,
            // Leave a little room above the first frame, as Windows does.
            stack_top: stack + stack_size - 0x100,
            teb,
            cmdline_a: params,
            cmdline_w: params + wide_off,
            layout,
            api_depth: 0,
            search_dirs,
            system_modules: Vec::new(),
            error_mode: 0,
        })
    }

    pub fn long_mode(&self) -> bool {
        self.ctx.long_mode
    }

    pub fn ptr_size(&self) -> usize {
        self.ctx.pointer_size()
    }

    /// Initialise DLLs (TLS callbacks, then the entry point), then the
    /// program's TLS callbacks and entry point. Returns the exit code.
    pub fn start(&mut self) -> Result<u32, RunError> {
        match self.start_inner() {
            Ok(code) | Err(Halt::Exit(code)) => Ok(code),
            Err(Halt::Error(e)) => Err(e),
        }
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
        self.run_tls_callbacks(0)?;
        let Some(entry) = self.process.modules[0].entry_point else {
            return Ok(0);
        };
        // Returning from the entry point ends the thread, and with it the
        // process, with the returned value as the exit code.
        let ret = self.call(entry, &[])?;
        Ok(ret as u32)
    }

    /// Run a DLL's TLS callbacks and entry point for `DLL_PROCESS_ATTACH`.
    /// Returns what the entry point returned (TRUE when it has none).
    pub fn init_dll(&mut self, module: usize) -> Result<bool, Halt> {
        let m = &self.process.modules[module];
        let (base, entry) = (m.base, m.entry_point);
        self.run_tls_callbacks(module)?;
        let Some(entry) = entry else {
            return Ok(true);
        };
        let ok = self.call(entry, &[base, DLL_PROCESS_ATTACH, 0])?;
        Ok(ok & 0xFFFF_FFFF != 0)
    }

    fn run_tls_callbacks(&mut self, module: usize) -> Result<(), Halt> {
        let m = &self.process.modules[module];
        let base = m.base;
        let callbacks = m
            .tls
            .as_ref()
            .map(|t| t.callbacks.clone())
            .unwrap_or_default();
        for cb in callbacks.into_iter().take_while(|&cb| cb != 0) {
            self.call(cb, &[base, DLL_PROCESS_ATTACH, 0])?;
        }
        Ok(())
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
        let result = self.call_from(target, args);
        self.stack_top = saved_top;
        self.ctx = saved;
        result
    }

    fn call_from(&mut self, target: u64, args: &[u64]) -> Result<u64, Halt> {
        let p = self.ptr_size();
        let mem = &mut self.process.memory;
        let mut sp = self.stack_top & !0xF;
        if self.ctx.long_mode {
            let slots = round_up(args.len().max(4) as u64 * 8, 16);
            sp -= slots;
            for (n, &a) in args.iter().enumerate() {
                match n {
                    0 => self.ctx.regs[x86_reg::RCX] = a,
                    1 => self.ctx.regs[x86_reg::RDX] = a,
                    2 => self.ctx.regs[x86_reg::R8] = a,
                    3 => self.ctx.regs[x86_reg::R9] = a,
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
        self.ctx.regs[x86_reg::RSP] = sp;
        self.ctx.rip = target;
        self.ctx.rflags = X86Context::INITIAL_FLAGS;

        loop {
            let mut gc =
                GuestContext::X86(std::mem::replace(&mut self.ctx, X86Context::new(false)));
            let budget = self.remaining();
            let stop = self.backend.run(&mut gc, &mut self.process.memory, budget);
            #[allow(unreachable_patterns)] // GuestContext is non_exhaustive.
            match gc {
                GuestContext::X86(c) => self.ctx = c,
                _ => unreachable!("the backend changed the guest architecture"),
            }
            match stop {
                StopReason::Breakpoint { address } if address == self.sentinel => {
                    return Ok(self.ctx.regs[x86_reg::RAX]);
                }
                StopReason::Breakpoint { address } => {
                    let Some(entry) = self.process.call_gate.lookup(address).cloned() else {
                        return Err(RunError::Breakpoint {
                            location: self.describe(address),
                        }
                        .into());
                    };
                    self.dispatch(&entry.dll, &entry.symbol)?;
                }
                StopReason::Fault(fault) => {
                    let location = self.describe(fault.pc());
                    return Err(RunError::Fault { fault, location }.into());
                }
                StopReason::InstructionLimit => {
                    return Err(RunError::InstructionLimit(self.limit.unwrap_or(0)).into());
                }
            }
        }
    }

    fn remaining(&self) -> Option<u64> {
        self.limit
            .map(|l| l.saturating_sub(self.backend.instructions_executed()))
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
        let args: Vec<u64> = (0..api.args)
            .map(|n| self.arg(n))
            .collect::<Result<_, _>>()?;
        self.api_depth += 1;
        let result = (api.f)(self, &args);
        self.api_depth -= 1;
        if self.trace {
            let shown: Vec<String> = args.iter().map(|a| format!("{a:#x}")).collect();
            let outcome = match &result {
                Ok(v) => format!("{v:#x}"),
                Err(Halt::Exit(code)) => format!("exit {code}"),
                Err(Halt::Error(_)) => "error".into(),
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
        let value = result?;
        self.ret(api, value)
    }

    /// Argument `n` of the API call the guest is making right now.
    fn arg(&self, n: u8) -> Result<u64, MemoryError> {
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
    /// the arguments.
    fn ret(&mut self, api: &Api, value: u64) -> Result<(), Halt> {
        let p = self.ptr_size() as u64;
        let sp = self.ctx.regs[x86_reg::RSP];
        let ret_addr = self.process.memory.read_ptr_raw(sp, p as usize)?;
        let mut new_sp = sp + p;
        if !self.ctx.long_mode {
            new_sp += api.args as u64 * 4;
            self.ctx.regs[x86_reg::RAX] = value & 0xFFFF_FFFF;
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
