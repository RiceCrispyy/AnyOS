//! The heap allocator behind `HeapAlloc`, `LocalAlloc` and `GlobalAlloc`.
//!
//! Only bookkeeping lives here; guest memory is mapped through a callback so
//! the allocator can be tested on its own. Small blocks are carved first-fit
//! out of segments and coalesced on free; large blocks get a mapping each.

use std::collections::BTreeMap;

/// Every block starts on a 16-byte boundary, as on 64-bit Windows.
pub(crate) const ALIGN: u64 = 16;
const SEGMENT: u64 = 0x10_0000;
/// Blocks at least this big get their own mapping.
const LARGE: u64 = 0x8_0000;

#[derive(Debug, Default)]
pub(crate) struct Heap {
    /// The `HANDLE` the guest sees: the base of the first segment.
    pub handle: u64,
    /// Free ranges in segments: start -> length.
    free: BTreeMap<u64, u64>,
    /// Live blocks: start -> (block length, requested size).
    used: BTreeMap<u64, (u64, u64)>,
    /// Large blocks, each its own mapping: start -> (mapping length, requested size).
    large: BTreeMap<u64, (u64, u64)>,
    pub segments: Vec<(u64, u64)>,
}

/// What a reallocation did, for the caller to copy and zero guest memory.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Realloc {
    /// Same address; the block grew or shrank in place.
    InPlace,
    /// New address; copy `copy` bytes from the old block, then call
    /// [`Heap::free`] on it.
    Moved { new: u64, copy: u64 },
}

fn block_len(size: u64) -> u64 {
    size.max(1).div_ceil(ALIGN) * ALIGN
}

impl Heap {
    /// A heap whose first segment is at `base` (`len` bytes).
    pub fn new(base: u64, len: u64) -> Heap {
        let mut h = Heap {
            handle: base,
            ..Heap::default()
        };
        h.add_segment(base, len);
        h
    }

    fn add_segment(&mut self, base: u64, len: u64) {
        self.segments.push((base, len));
        self.insert_free(base, len);
    }

    /// Size of segment to map for a block of `size` bytes.
    pub fn segment_size(size: u64) -> u64 {
        block_len(size).max(SEGMENT).div_ceil(0x1_0000) * 0x1_0000
    }

    /// Allocate `size` bytes. `map(len)` maps fresh zeroed memory and returns
    /// its base. Returns the address and whether the memory is known to be
    /// zero already.
    pub fn alloc(
        &mut self,
        size: u64,
        mut map: impl FnMut(u64) -> Option<u64>,
    ) -> Option<(u64, bool)> {
        let len = block_len(size);
        if len >= LARGE {
            let map_len = len.div_ceil(0x1000) * 0x1000;
            let p = map(map_len)?;
            self.large.insert(p, (map_len, size));
            return Some((p, true));
        }
        let fresh = match self.free.iter().find(|&(_, &l)| l >= len).map(|(&a, _)| a) {
            Some(a) => a,
            None => {
                let seg = Heap::segment_size(len);
                let base = map(seg)?;
                self.add_segment(base, seg);
                base
            }
        };
        let avail = self.free.remove(&fresh)?;
        if avail > len {
            self.free.insert(fresh + len, avail - len);
        }
        self.used.insert(fresh, (len, size));
        Some((fresh, false))
    }

    /// Free the block at `addr`. Returns the mapping to unmap for a large block.
    pub fn free(&mut self, addr: u64) -> Result<Option<u64>, ()> {
        if self.large.remove(&addr).is_some() {
            return Ok(Some(addr));
        }
        let (len, _) = self.used.remove(&addr).ok_or(())?;
        self.insert_free(addr, len);
        Ok(None)
    }

    fn insert_free(&mut self, mut addr: u64, mut len: u64) {
        if let Some((&next, &nlen)) = self.free.range(addr + len..).next() {
            if next == addr + len {
                self.free.remove(&next);
                len += nlen;
            }
        }
        if let Some((&prev, &plen)) = self.free.range(..addr).next_back() {
            if prev + plen == addr {
                self.free.remove(&prev);
                addr = prev;
                len += plen;
            }
        }
        self.free.insert(addr, len);
    }

    /// The size the block at `addr` was allocated with.
    pub fn size(&self, addr: u64) -> Option<u64> {
        self.used
            .get(&addr)
            .or_else(|| self.large.get(&addr))
            .map(|&(_, size)| size)
    }

    pub fn owns(&self, addr: u64) -> bool {
        self.used.contains_key(&addr) || self.large.contains_key(&addr)
    }

    /// Resize the block at `addr` to `size`. With `in_place_only`, fail rather
    /// than move.
    pub fn realloc(
        &mut self,
        addr: u64,
        size: u64,
        in_place_only: bool,
        map: impl FnMut(u64) -> Option<u64>,
    ) -> Option<Realloc> {
        let new_len = block_len(size);
        if let Some(&(map_len, old)) = self.large.get(&addr) {
            if new_len <= map_len {
                self.large.insert(addr, (map_len, size));
                return Some(Realloc::InPlace);
            }
            if in_place_only {
                return None;
            }
            let (new, _) = self.alloc(size, map)?;
            return Some(Realloc::Moved {
                new,
                copy: old.min(size),
            });
        }
        let &(len, old) = self.used.get(&addr)?;
        if new_len <= len {
            // Shrink, handing the tail back when it is worth keeping.
            if len - new_len >= ALIGN * 2 {
                self.used.insert(addr, (new_len, size));
                self.insert_free(addr + new_len, len - new_len);
            } else {
                self.used.insert(addr, (len, size));
            }
            return Some(Realloc::InPlace);
        }
        // Grow into the free range right after the block.
        if new_len < LARGE {
            let end = addr + len;
            if let Some(&next_len) = self.free.get(&end) {
                if len + next_len >= new_len {
                    self.free.remove(&end);
                    let rest = len + next_len - new_len;
                    if rest > 0 {
                        self.free.insert(addr + new_len, rest);
                    }
                    self.used.insert(addr, (new_len, size));
                    return Some(Realloc::InPlace);
                }
            }
        }
        if in_place_only {
            return None;
        }
        let (new, _) = self.alloc(size, map)?;
        Some(Realloc::Moved {
            new,
            copy: old.min(size),
        })
    }

    /// The largest free block, for `HeapCompact`.
    pub fn largest_free(&self) -> u64 {
        self.free.values().copied().max().unwrap_or(0)
    }

    /// Every mapping the heap owns, for `HeapDestroy`.
    pub fn mappings(&self) -> Vec<u64> {
        self.segments
            .iter()
            .map(|s| s.0)
            .chain(self.large.keys().copied())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hands out fake mappings at increasing addresses.
    fn mapper() -> impl FnMut(u64) -> Option<u64> {
        let mut next = 0x1000_0000u64;
        move |len| {
            let p = next;
            next += len.div_ceil(0x1_0000) * 0x1_0000;
            Some(p)
        }
    }

    #[test]
    fn blocks_are_aligned_reused_and_coalesced() {
        let mut map = mapper();
        let base = map(SEGMENT).unwrap();
        let mut h = Heap::new(base, SEGMENT);
        let (a, _) = h.alloc(10, &mut map).unwrap();
        let (b, _) = h.alloc(100, &mut map).unwrap();
        let (c, _) = h.alloc(1, &mut map).unwrap();
        assert_eq!(a % ALIGN, 0);
        assert_eq!(b, a + 16);
        assert_eq!(c, b + 112);
        assert_eq!(h.size(b), Some(100));
        h.free(a).unwrap();
        h.free(b).unwrap();
        // a and b coalesced: a 128-byte block fits where they were.
        let (d, _) = h.alloc(128, &mut map).unwrap();
        assert_eq!(d, a);
        assert!(h.free(0x1234).is_err());
        assert!(h.free(d).is_ok());
        assert!(h.free(d).is_err(), "double free is caught");
    }

    #[test]
    fn realloc_grows_in_place_or_moves() {
        let mut map = mapper();
        let base = map(SEGMENT).unwrap();
        let mut h = Heap::new(base, SEGMENT);
        let (a, _) = h.alloc(16, &mut map).unwrap();
        assert_eq!(h.realloc(a, 64, false, &mut map), Some(Realloc::InPlace));
        assert_eq!(h.size(a), Some(64));
        let (b, _) = h.alloc(16, &mut map).unwrap();
        assert_eq!(b, a + 64);
        assert_eq!(h.realloc(a, 200, true, &mut map), None);
        let Some(Realloc::Moved { new, copy }) = h.realloc(a, 200, false, &mut map) else {
            panic!("expected a move");
        };
        assert_eq!(copy, 64);
        assert!(new > b);
        h.free(a).unwrap();
        assert_eq!(h.realloc(new, 8, false, &mut map), Some(Realloc::InPlace));
    }

    #[test]
    fn large_blocks_get_their_own_mapping() {
        let mut map = mapper();
        let base = map(SEGMENT).unwrap();
        let mut h = Heap::new(base, SEGMENT);
        let (big, zeroed) = h.alloc(LARGE * 2, &mut map).unwrap();
        assert!(zeroed);
        assert!(big >= base + SEGMENT);
        assert_eq!(h.free(big), Ok(Some(big)));
        // Segments grow when a small block does not fit.
        for _ in 0..(SEGMENT / 0x4_0000 + 1) {
            h.alloc(0x3_FFF0, &mut map).unwrap();
        }
        assert!(h.segments.len() > 1);
    }
}
