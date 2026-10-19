//! Guest memory region management and access.

use anyos_memory::{AddressSpace, MemoryError, Protection};

/// Information about a mapped guest memory region.
#[derive(Debug, Clone)]
pub struct GuestRegion {
    /// Base virtual address of the region
    pub base: u64,
    /// Size of the region in bytes
    pub size: u64,
    /// Tag describing the region's purpose
    pub tag: String,
    /// Current protection flags for the region
    pub protection: Protection,
}

impl GuestRegion {
    /// Create a new guest region descriptor.
    pub fn new(base: u64, size: u64, tag: &str, protection: Protection) -> Self {
        GuestRegion {
            base,
            size,
            tag: tag.to_string(),
            protection,
        }
    }

    /// Get the end address of this region.
    pub fn end(&self) -> u64 {
        self.base + self.size
    }

    /// Check if an address is within this region.
    pub fn contains(&self, addr: u64) -> bool {
        addr >= self.base && addr < self.end()
    }

    /// Get the raw bytes from the region (without bounds checking).
    /// This is a helper for converting region info to data retrieval.
    /// Actual data access must go through the AddressSpace.
    pub fn offset_range(&self, offset: u64, size: u64) -> Option<(u64, u64)> {
        if offset >= self.size {
            return None;
        }
        let available = self.size - offset;
        let read_size = size.min(available);
        Some((self.base + offset, read_size))
    }
}

/// High-level API for guest memory operations.
pub struct GuestMemory {
    space: AddressSpace,
}

impl GuestMemory {
    /// Create a new guest memory model with Windows user-mode address space.
    pub fn new_windows_user() -> Self {
        let space = AddressSpace::windows_user(8, false);
        GuestMemory { space }
    }

    /// Get a reference to the underlying address space.
    pub fn space(&self) -> &AddressSpace {
        &self.space
    }

    /// Get a mutable reference to the underlying address space.
    pub fn space_mut(&mut self) -> &mut AddressSpace {
        &mut self.space
    }

    /// Map a region at a specific address.
    pub fn map_region(
        &mut self,
        base: u64,
        size: u64,
        prot: Protection,
        tag: &str,
    ) -> Result<(), MemoryError> {
        self.space.map(base, size, prot, tag)
    }

    /// Unmap a region.
    pub fn unmap_region(&mut self, base: u64) -> Result<(), MemoryError> {
        self.space.unmap(base)
    }

    /// Read data from guest memory (enforces protections).
    pub fn read(&self, addr: u64, buf: &mut [u8]) -> Result<(), MemoryError> {
        self.space.read(addr, buf)
    }

    /// Write data to guest memory (enforces protections).
    pub fn write(&mut self, addr: u64, data: &[u8]) -> Result<(), MemoryError> {
        self.space.write(addr, data)
    }

    /// Privileged read (ignores protections).
    pub fn read_raw(&self, addr: u64, buf: &mut [u8]) -> Result<(), MemoryError> {
        self.space.read_raw(addr, buf)
    }

    /// Privileged write (ignores protections).
    pub fn write_raw(&mut self, addr: u64, data: &[u8]) -> Result<(), MemoryError> {
        self.space.write_raw(addr, data)
    }

    /// Get information about all mapped regions.
    pub fn regions(&self) -> Vec<GuestRegion> {
        self.space
            .regions()
            .into_iter()
            .map(|r| {
                // Determine the overall protection of the region
                // (typically the first page's protection)
                let prot = self.space.protection_at(r.base).unwrap_or(Protection::NONE);
                GuestRegion {
                    base: r.base,
                    size: r.size,
                    tag: r.tag,
                    protection: prot,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_region_contains() {
        let r = GuestRegion::new(0x1000, 0x2000, "test", Protection::RW);
        assert!(r.contains(0x1000));
        assert!(r.contains(0x2000));
        assert!(r.contains(0x2fff));
        assert!(!r.contains(0x3000));
        assert!(!r.contains(0xfff));
    }

    #[test]
    fn guest_memory_operations() {
        let mut mem = GuestMemory::new_windows_user();
        mem.map_region(0x10_0000, 0x1000, Protection::RW, "test")
            .unwrap();

        let data = [1, 2, 3, 4];
        mem.write(0x10_0000, &data).unwrap();

        let mut buf = [0; 4];
        mem.read(0x10_0000, &mut buf).unwrap();
        assert_eq!(buf, data);
    }
}
