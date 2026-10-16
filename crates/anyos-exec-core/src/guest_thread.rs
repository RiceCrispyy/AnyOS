//! Guest thread state and execution.

#[cfg(target_arch = "x86_64")]
use crate::error::ExecResult;
use parking_lot::Mutex;
#[cfg(target_arch = "x86_64")]
use std::arch::asm;
use std::sync::Arc;

/// CPU registers available to guest code.
#[derive(Debug, Clone, Default)]
pub struct GuestRegs {
    // x86-64 general-purpose registers
    #[cfg(target_arch = "x86_64")]
    pub rax: u64,
    #[cfg(target_arch = "x86_64")]
    pub rbx: u64,
    #[cfg(target_arch = "x86_64")]
    pub rcx: u64,
    #[cfg(target_arch = "x86_64")]
    pub rdx: u64,
    #[cfg(target_arch = "x86_64")]
    pub rsi: u64,
    #[cfg(target_arch = "x86_64")]
    pub rdi: u64,
    #[cfg(target_arch = "x86_64")]
    pub rbp: u64,
    #[cfg(target_arch = "x86_64")]
    pub rsp: u64,
    #[cfg(target_arch = "x86_64")]
    pub r8: u64,
    #[cfg(target_arch = "x86_64")]
    pub r9: u64,
    #[cfg(target_arch = "x86_64")]
    pub r10: u64,
    #[cfg(target_arch = "x86_64")]
    pub r11: u64,
    #[cfg(target_arch = "x86_64")]
    pub r12: u64,
    #[cfg(target_arch = "x86_64")]
    pub r13: u64,
    #[cfg(target_arch = "x86_64")]
    pub r14: u64,
    #[cfg(target_arch = "x86_64")]
    pub r15: u64,
    #[cfg(target_arch = "x86_64")]
    pub rip: u64,

    // AArch64 registers
    #[cfg(target_arch = "aarch64")]
    pub x: [u64; 31],
    #[cfg(target_arch = "aarch64")]
    pub sp: u64,
    #[cfg(target_arch = "aarch64")]
    pub pc: u64,
}

/// A guest thread execution context.
pub struct GuestThread {
    /// Current thread ID
    id: u64,
    /// CPU register state
    regs: Arc<Mutex<GuestRegs>>,
    /// Thread-local storage pointer
    tls: u64,
}

impl GuestThread {
    /// Create a new guest thread.
    pub fn new(id: u64) -> Self {
        GuestThread {
            id,
            regs: Arc::new(Mutex::new(GuestRegs::default())),
            tls: 0,
        }
    }

    /// Get the thread ID.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Get a clone of the register state.
    pub fn get_regs(&self) -> GuestRegs {
        self.regs.lock().clone()
    }

    /// Set the register state.
    pub fn set_regs(&self, regs: GuestRegs) {
        *self.regs.lock() = regs;
    }

    /// Get the thread-local storage pointer.
    pub fn tls(&self) -> u64 {
        self.tls
    }

    /// Set the thread-local storage pointer.
    pub fn set_tls(&mut self, tls: u64) {
        self.tls = tls;
    }

    /// Run guest code at the given entry point.
    /// This simulates executing guest code and returns the exit code.
    #[cfg(target_arch = "x86_64")]
    pub fn run_entry(&self, entry: u64, stack_ptr: u64) -> ExecResult<u64> {
        let mut regs = self.regs.lock();

        // Initialize registers for entry
        regs.rip = entry;
        regs.rsp = stack_ptr;
        regs.rbp = stack_ptr;

        // In a real implementation, we would:
        // 1. Save the host's register state
        // 2. Load the guest registers
        // 3. Execute guest code until a syscall or exit
        // 4. Restore the host's register state
        //
        // For now, we just return success. A real implementation would involve
        // unsafe inline assembly to transfer control to guest code.

        // This is a placeholder that demonstrates the pattern.
        // The actual implementation would use inline assembly like:
        //
        // unsafe {
        //     asm!(
        //         "mov {rip}, %rax",
        //         "mov {rsp}, %rsp",
        //         "jmp %rax",
        //         rip = in(reg) regs.rip,
        //         rsp = in(reg) regs.rsp,
        //         options(noreturn)
        //     );
        // }

        Ok(0)
    }

    /// Save the current host register state (x86-64 only).
    /// This is used internally to preserve host context during guest execution.
    #[allow(dead_code)] // placeholder: wired up once guest execution lands
    #[cfg(target_arch = "x86_64")]
    fn save_host_state() -> ExecResult<u64> {
        // Get the current stack pointer
        let rsp: u64;

        unsafe {
            // SAFETY: This inline assembly reads the stack pointer, which is safe.
            // The syntax is: "mov {}, {}" with register constraints.
            // The correct x86-64 syntax for storing rsp is:
            // asm!("mov %rsp, {}", out(reg) rsp)
            asm!("mov %rsp, {0}", out(reg) rsp);
        }

        Ok(rsp)
    }
}

impl Clone for GuestThread {
    fn clone(&self) -> Self {
        GuestThread {
            id: self.id,
            regs: Arc::clone(&self.regs),
            tls: self.tls,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_thread_creation() {
        let thread = GuestThread::new(1);
        assert_eq!(thread.id(), 1);
        assert_eq!(thread.tls(), 0);
    }

    #[test]
    fn guest_thread_registers() {
        let thread = GuestThread::new(1);
        let mut regs = GuestRegs::default();
        #[cfg(target_arch = "x86_64")]
        {
            regs.rax = 42;
        }
        thread.set_regs(regs);
        let read = thread.get_regs();
        #[cfg(target_arch = "x86_64")]
        {
            assert_eq!(read.rax, 42);
        }
    }
}
