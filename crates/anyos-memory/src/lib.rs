//! # anyos-memory
//!
//! A model of a guest (Windows) process address space: page-granular regions with
//! Windows-style protections and checked reads/writes.
//!
//! This first backend keeps guest memory in host heap buffers, which is what the
//! loader and an interpreter need. A native backend (host `mmap` reservations at
//! the guest's own addresses, needed for direct execution) will implement the same
//! operations later. Note that this model is not a security sandbox by itself.
//!
//! Two access paths exist on purpose:
//! * [`AddressSpace::read`] / [`AddressSpace::write`] / [`AddressSpace::check_execute`]
//!   enforce page protections, as guest code would experience them.
//! * `*_raw` variants bypass protections for the loader and runtime services
//!   (which, like the Windows kernel, may write to read-only image pages), but
//!   still require the memory to be mapped.

use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;

/// Guest page size. Windows uses 4 KiB pages on x86-64 and ARM64.
pub const PAGE_SIZE: u64 = 0x1000;
/// Windows reserves address space in 64 KiB units.
pub const ALLOCATION_GRANULARITY: u64 = 0x1_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct Protection {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

impl Protection {
    pub const NONE: Self = Protection {
        read: false,
        write: false,
        execute: false,
    };
    pub const R: Self = Protection {
        read: true,
        write: false,
        execute: false,
    };
    pub const RW: Self = Protection {
        read: true,
        write: true,
        execute: false,
    };
    pub const RX: Self = Protection {
        read: true,
        write: false,
        execute: true,
    };
    pub const RWX: Self = Protection {
        read: true,
        write: true,
        execute: true,
    };
    pub const X: Self = Protection {
        read: false,
        write: false,
        execute: true,
    };

    pub fn allows(self, access: Access) -> bool {
        match access {
            Access::Read => self.read,
            Access::Write => self.write,
            Access::Execute => self.execute,
        }
    }
}

impl fmt::Display for Protection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}{}{}",
            if self.read { 'r' } else { '-' },
            if self.write { 'w' } else { '-' },
            if self.execute { 'x' } else { '-' }
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Access {
    Read,
    Write,
    Execute,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MemoryError {
    #[error("address 0x{addr:x} is not mapped")]
    Unmapped { addr: u64 },
    #[error("{access:?} access violation at 0x{addr:x}")]
    AccessViolation { addr: u64, access: Access },
    #[error("range 0x{base:x}+0x{size:x} overlaps an existing mapping")]
    Overlap { base: u64, size: u64 },
    #[error("range 0x{base:x}+0x{size:x} is outside the address space (0x{low:x}..0x{high:x})")]
    OutOfRange {
        base: u64,
        size: u64,
        low: u64,
        high: u64,
    },
    #[error("0x{0:x} is not page aligned")]
    Misaligned(u64),
    #[error("mapping size must be non-zero")]
    ZeroSize,
    #[error("no free range of 0x{size:x} bytes")]
    Exhausted { size: u64 },
    #[error("memory budget exceeded: 0x{requested:x} more bytes requested, 0x{remaining:x} left")]
    BudgetExceeded { requested: u64, remaining: u64 },
}

struct Region {
    size: u64,
    tag: String,
    data: Vec<u8>,
    /// One entry per page.
    prot: Vec<Protection>,
}

/// Describes one mapped region.
#[derive(Debug, Clone, Serialize)]
pub struct RegionInfo {
    pub base: u64,
    pub size: u64,
    pub tag: String,
    /// Protection runs within the region: (offset, length, protection).
    pub protections: Vec<(u64, u64, Protection)>,
}

/// A guest address space.
pub struct AddressSpace {
    regions: BTreeMap<u64, Region>,
    low: u64,
    high: u64,
    budget: u64,
    used: u64,
}

impl fmt::Debug for AddressSpace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AddressSpace")
            .field("low", &self.low)
            .field("high", &self.high)
            .field("regions", &self.regions.len())
            .field("used", &self.used)
            .finish()
    }
}

impl AddressSpace {
    /// Address space covering `[low, high)` with a cap on total mapped bytes.
    pub fn new(low: u64, high: u64, budget: u64) -> Self {
        AddressSpace {
            regions: BTreeMap::new(),
            low,
            high,
            budget,
            used: 0,
        }
    }

    /// Windows user-mode layout: 64-bit processes get 128 TiB, 32-bit get 2 GiB
    /// (4 GiB when large-address-aware). The first 64 KiB is never mapped.
    pub fn windows_user(pointer_size: usize, large_address_aware: bool) -> Self {
        let high = match (pointer_size, large_address_aware) {
            (8, _) => 0x7FFF_FFFF_0000,
            (_, true) => 0xFFFF_0000,
            _ => 0x7FFF_0000,
        };
        AddressSpace::new(ALLOCATION_GRANULARITY, high, 4 << 30)
    }

    pub fn bounds(&self) -> (u64, u64) {
        (self.low, self.high)
    }

    pub fn mapped_bytes(&self) -> u64 {
        self.used
    }

    fn check_range(&self, base: u64, size: u64) -> Result<(), MemoryError> {
        if size == 0 {
            return Err(MemoryError::ZeroSize);
        }
        if !base.is_multiple_of(PAGE_SIZE) {
            return Err(MemoryError::Misaligned(base));
        }
        let end = base.checked_add(size);
        if base < self.low || end.is_none_or(|e| e > self.high) {
            return Err(MemoryError::OutOfRange {
                base,
                size,
                low: self.low,
                high: self.high,
            });
        }
        Ok(())
    }

    /// True when `[base, base+size)` is inside the address space and unmapped.
    pub fn is_free(&self, base: u64, size: u64) -> bool {
        let size = round_up(size, PAGE_SIZE);
        if self.check_range(base, size).is_err() {
            return false;
        }
        let end = base + size;
        if let Some((&b, r)) = self.regions.range(..end).next_back() {
            if b + r.size > base {
                return false;
            }
        }
        true
    }

    /// Map zeroed memory at a fixed, page-aligned address. Size is rounded up to pages.
    pub fn map(
        &mut self,
        base: u64,
        size: u64,
        prot: Protection,
        tag: &str,
    ) -> Result<(), MemoryError> {
        let size = round_up(size, PAGE_SIZE);
        self.check_range(base, size)?;
        if !self.is_free(base, size) {
            return Err(MemoryError::Overlap { base, size });
        }
        let remaining = self.budget - self.used;
        if size > remaining {
            return Err(MemoryError::BudgetExceeded {
                requested: size,
                remaining,
            });
        }
        let pages = (size / PAGE_SIZE) as usize;
        self.regions.insert(
            base,
            Region {
                size,
                tag: tag.to_string(),
                data: vec![0; size as usize],
                prot: vec![prot; pages],
            },
        );
        self.used += size;
        Ok(())
    }

    /// Map at the lowest free 64 KiB-aligned address at or above `hint` (or the
    /// bottom of the address space) and return the base.
    pub fn map_anywhere(
        &mut self,
        size: u64,
        prot: Protection,
        tag: &str,
        hint: Option<u64>,
    ) -> Result<u64, MemoryError> {
        let size = round_up(size, PAGE_SIZE);
        if size == 0 {
            return Err(MemoryError::ZeroSize);
        }
        let mut candidate = round_up(
            hint.unwrap_or(self.low).max(self.low),
            ALLOCATION_GRANULARITY,
        );
        loop {
            let end = candidate
                .checked_add(size)
                .ok_or(MemoryError::Exhausted { size })?;
            if end > self.high {
                return Err(MemoryError::Exhausted { size });
            }
            // The last region starting before `end` is the only one that can overlap.
            match self.regions.range(..end).next_back() {
                Some((&b, r)) if b + r.size > candidate => {
                    candidate = round_up(b + r.size, ALLOCATION_GRANULARITY);
                }
                _ => {
                    self.map(candidate, size, prot, tag)?;
                    return Ok(candidate);
                }
            }
        }
    }

    /// Remove the region that starts at `base`.
    pub fn unmap(&mut self, base: u64) -> Result<(), MemoryError> {
        let r = self
            .regions
            .remove(&base)
            .ok_or(MemoryError::Unmapped { addr: base })?;
        self.used -= r.size;
        Ok(())
    }

    fn region_at(&self, addr: u64) -> Result<(u64, &Region), MemoryError> {
        match self.regions.range(..=addr).next_back() {
            Some((&b, r)) if addr < b + r.size => Ok((b, r)),
            _ => Err(MemoryError::Unmapped { addr }),
        }
    }

    fn region_at_mut(&mut self, addr: u64) -> Result<(u64, &mut Region), MemoryError> {
        match self.regions.range_mut(..=addr).next_back() {
            Some((&b, r)) if addr < b + r.size => Ok((b, r)),
            _ => Err(MemoryError::Unmapped { addr }),
        }
    }

    /// Change protection of whole pages covering `[addr, addr+size)`. The range may
    /// not span regions. Returns the previous protection of the first page.
    pub fn protect(
        &mut self,
        addr: u64,
        size: u64,
        prot: Protection,
    ) -> Result<Protection, MemoryError> {
        if size == 0 {
            return Err(MemoryError::ZeroSize);
        }
        let first = round_down(addr, PAGE_SIZE);
        let last = round_up(
            addr.checked_add(size)
                .ok_or(MemoryError::Unmapped { addr })?,
            PAGE_SIZE,
        );
        let (base, r) = self.region_at_mut(first)?;
        if last > base + r.size {
            return Err(MemoryError::Unmapped {
                addr: base + r.size,
            });
        }
        let p0 = ((first - base) / PAGE_SIZE) as usize;
        let p1 = ((last - base) / PAGE_SIZE) as usize;
        let old = r.prot[p0];
        r.prot[p0..p1].fill(prot);
        Ok(old)
    }

    /// Protection of the page containing `addr`.
    pub fn protection_at(&self, addr: u64) -> Result<Protection, MemoryError> {
        let (base, r) = self.region_at(addr)?;
        Ok(r.prot[((addr - base) / PAGE_SIZE) as usize])
    }

    pub fn query(&self, addr: u64) -> Option<RegionInfo> {
        self.region_at(addr).ok().map(|(b, r)| info(b, r))
    }

    pub fn regions(&self) -> Vec<RegionInfo> {
        self.regions.iter().map(|(&b, r)| info(b, r)).collect()
    }

    /// The byte spans covering `[addr, addr+len)`, which may cross adjacent regions.
    /// `check` is applied to every page touched (None = privileged access).
    fn spans(
        &self,
        addr: u64,
        len: usize,
        check: Option<Access>,
    ) -> Result<Vec<(u64, usize, usize)>, MemoryError> {
        let mut out = Vec::new();
        let mut cur = addr;
        let end = addr
            .checked_add(len as u64)
            .ok_or(MemoryError::Unmapped { addr })?;
        while cur < end {
            let (base, r) = self.region_at(cur)?;
            let chunk_end = end.min(base + r.size);
            if let Some(access) = check {
                let p0 = ((cur - base) / PAGE_SIZE) as usize;
                let p1 = ((chunk_end - 1 - base) / PAGE_SIZE) as usize;
                if let Some(bad) = (p0..=p1).find(|&p| !r.prot[p].allows(access)) {
                    let fault = (base + bad as u64 * PAGE_SIZE).max(cur);
                    return Err(MemoryError::AccessViolation {
                        addr: fault,
                        access,
                    });
                }
            }
            out.push((base, (cur - base) as usize, (chunk_end - cur) as usize));
            cur = chunk_end;
        }
        Ok(out)
    }

    fn read_with(
        &self,
        addr: u64,
        buf: &mut [u8],
        check: Option<Access>,
    ) -> Result<(), MemoryError> {
        let mut done = 0;
        for (base, off, n) in self.spans(addr, buf.len(), check)? {
            buf[done..done + n].copy_from_slice(&self.regions[&base].data[off..off + n]);
            done += n;
        }
        Ok(())
    }

    fn write_with(
        &mut self,
        addr: u64,
        data: &[u8],
        check: Option<Access>,
    ) -> Result<(), MemoryError> {
        let mut done = 0;
        for (base, off, n) in self.spans(addr, data.len(), check)? {
            let r = self
                .regions
                .get_mut(&base)
                .expect("span from existing region");
            r.data[off..off + n].copy_from_slice(&data[done..done + n]);
            done += n;
        }
        Ok(())
    }

    /// Guest read: requires read permission on every page touched.
    pub fn read(&self, addr: u64, buf: &mut [u8]) -> Result<(), MemoryError> {
        self.read_with(addr, buf, Some(Access::Read))
    }

    /// Guest write: requires write permission on every page touched.
    pub fn write(&mut self, addr: u64, data: &[u8]) -> Result<(), MemoryError> {
        self.write_with(addr, data, Some(Access::Write))
    }

    /// Fails unless the page containing `addr` is mapped executable.
    pub fn check_execute(&self, addr: u64) -> Result<(), MemoryError> {
        self.spans(addr, 1, Some(Access::Execute)).map(|_| ())
    }

    /// Privileged read (loader/runtime): ignores protections.
    pub fn read_raw(&self, addr: u64, buf: &mut [u8]) -> Result<(), MemoryError> {
        self.read_with(addr, buf, None)
    }

    /// Privileged write (loader/runtime): ignores protections.
    pub fn write_raw(&mut self, addr: u64, data: &[u8]) -> Result<(), MemoryError> {
        self.write_with(addr, data, None)
    }

    pub fn read_u16_raw(&self, addr: u64) -> Result<u16, MemoryError> {
        let mut b = [0; 2];
        self.read_raw(addr, &mut b)?;
        Ok(u16::from_le_bytes(b))
    }

    pub fn read_u32_raw(&self, addr: u64) -> Result<u32, MemoryError> {
        let mut b = [0; 4];
        self.read_raw(addr, &mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    pub fn read_u64_raw(&self, addr: u64) -> Result<u64, MemoryError> {
        let mut b = [0; 8];
        self.read_raw(addr, &mut b)?;
        Ok(u64::from_le_bytes(b))
    }

    /// Pointer-sized little-endian read (4 or 8 bytes).
    pub fn read_ptr_raw(&self, addr: u64, ptr_size: usize) -> Result<u64, MemoryError> {
        if ptr_size == 8 {
            self.read_u64_raw(addr)
        } else {
            self.read_u32_raw(addr).map(u64::from)
        }
    }

    /// Pointer-sized little-endian write; the value is truncated for 4-byte pointers.
    pub fn write_ptr_raw(
        &mut self,
        addr: u64,
        ptr_size: usize,
        value: u64,
    ) -> Result<(), MemoryError> {
        if ptr_size == 8 {
            self.write_raw(addr, &value.to_le_bytes())
        } else {
            self.write_raw(addr, &(value as u32).to_le_bytes())
        }
    }

    /// Read a NUL-terminated byte string of at most `max` bytes (guest-checked).
    pub fn read_cstr(&self, addr: u64, max: usize) -> Result<Vec<u8>, MemoryError> {
        let mut out = Vec::new();
        let mut b = [0u8; 1];
        for i in 0..max as u64 {
            self.read(addr + i, &mut b)?;
            if b[0] == 0 {
                break;
            }
            out.push(b[0]);
        }
        Ok(out)
    }
}

fn info(base: u64, r: &Region) -> RegionInfo {
    let mut protections: Vec<(u64, u64, Protection)> = Vec::new();
    for (i, p) in r.prot.iter().enumerate() {
        let off = i as u64 * PAGE_SIZE;
        match protections.last_mut() {
            Some((_, len, last)) if last == p => *len += PAGE_SIZE,
            _ => protections.push((off, PAGE_SIZE, *p)),
        }
    }
    RegionInfo {
        base,
        size: r.size,
        tag: r.tag.clone(),
        protections,
    }
}

pub fn round_up(v: u64, align: u64) -> u64 {
    debug_assert!(align.is_power_of_two());
    v.checked_add(align - 1)
        .map_or(!(align - 1), |x| x & !(align - 1))
}

pub fn round_down(v: u64, align: u64) -> u64 {
    v & !(align - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn space() -> AddressSpace {
        AddressSpace::new(0x10000, 0x1_0000_0000, 64 << 20)
    }

    #[test]
    fn map_read_write_and_protections() {
        let mut m = space();
        m.map(0x40_0000, 0x2000, Protection::RW, "data").unwrap();
        m.write(0x40_0ffe, &[1, 2, 3, 4]).unwrap(); // crosses a page boundary
        let mut buf = [0; 4];
        m.read(0x40_0ffe, &mut buf).unwrap();
        assert_eq!(buf, [1, 2, 3, 4]);

        m.protect(0x40_1000, 1, Protection::R).unwrap();
        assert_eq!(
            m.write(0x40_0ffe, &[9; 4]),
            Err(MemoryError::AccessViolation {
                addr: 0x40_1000,
                access: Access::Write
            })
        );
        // Privileged writes ignore protection.
        m.write_raw(0x40_1000, &[7]).unwrap();
        assert_eq!(m.read_u32_raw(0x40_0fff).unwrap() & 0xff00, 0x0700);
        assert!(m.check_execute(0x40_0000).is_err());
    }

    #[test]
    fn rejects_overlap_out_of_range_and_unmapped() {
        let mut m = space();
        m.map(0x40_0000, 0x1000, Protection::R, "a").unwrap();
        assert!(matches!(
            m.map(0x40_0000, 0x1000, Protection::R, "b"),
            Err(MemoryError::Overlap { .. })
        ));
        assert!(matches!(
            m.map(0x1000, 0x1000, Protection::R, "low"),
            Err(MemoryError::OutOfRange { .. })
        ));
        assert!(matches!(
            m.map(0xFFFF_F000, 0x2000, Protection::R, "high"),
            Err(MemoryError::OutOfRange { .. })
        ));
        assert!(matches!(
            m.map(0x50_0001, 0x1000, Protection::R, "mis"),
            Err(MemoryError::Misaligned(_))
        ));
        let mut b = [0; 1];
        assert_eq!(
            m.read(0x40_1000, &mut b),
            Err(MemoryError::Unmapped { addr: 0x40_1000 })
        );
    }

    #[test]
    fn map_anywhere_skips_used_ranges_and_respects_budget() {
        let mut m = space();
        m.map(0x10000, 0x30000, Protection::R, "a").unwrap();
        let b = m.map_anywhere(0x1000, Protection::RW, "b", None).unwrap();
        assert_eq!(b, 0x40000);
        assert!(matches!(
            m.map_anywhere(128 << 20, Protection::RW, "huge", None),
            Err(MemoryError::BudgetExceeded { .. })
        ));
        m.unmap(0x10000).unwrap();
        assert_eq!(
            m.map_anywhere(0x1000, Protection::RW, "c", None).unwrap(),
            0x10000
        );
    }

    #[test]
    fn region_info_merges_protection_runs() {
        let mut m = space();
        m.map(0x40_0000, 0x4000, Protection::R, "img").unwrap();
        m.protect(0x40_1000, 0x2000, Protection::RX).unwrap();
        let info = m.query(0x40_2000).unwrap();
        assert_eq!(
            info.protections,
            vec![
                (0, 0x1000, Protection::R),
                (0x1000, 0x2000, Protection::RX),
                (0x3000, 0x1000, Protection::R)
            ]
        );
    }
}
