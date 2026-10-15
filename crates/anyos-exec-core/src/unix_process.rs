//! Unix process management integration.

use crate::error::ExecResult;
use anyos_memory::AddressSpace;

/// Unix process wrapper for guest execution.
pub struct UnixProcess {
    /// Process ID
    pid: u32,
    /// Guest address space
    space: AddressSpace,
}

impl UnixProcess {
    /// Create a new Unix process wrapper.
    pub fn new(pid: u32) -> Self {
        UnixProcess {
            pid,
            space: AddressSpace::windows_user(8, false),
        }
    }

    /// Get the process ID.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Get a reference to the address space.
    pub fn space(&self) -> &AddressSpace {
        &self.space
    }

    /// Get a mutable reference to the address space.
    pub fn space_mut(&mut self) -> &mut AddressSpace {
        &mut self.space
    }

    /// Wait for the process to exit and get the exit code.
    pub fn wait(&self) -> ExecResult<i32> {
        // In a real implementation, this would wait for the child process
        // to terminate and collect its exit code.
        // For now, return success.
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_creation() {
        let proc = UnixProcess::new(1234);
        assert_eq!(proc.pid(), 1234);
    }
}
