//! Memory APIs: `Virtual*`, `Heap*`, `Local*` and `Global*`.
//!
//! `VirtualAlloc` reservations map their whole range up front with no access
//! and track which pages are committed, so `VirtualQuery` reports reserved
//! and committed pages the way Windows does. Heaps are [`crate::heap::Heap`]
//! allocators over guest mappings; `LocalAlloc` and `GlobalAlloc` use the
//! process heap and treat movable memory as fixed (a lock returns the handle).

use anyos_memory::{round_down, round_up, Protection, ALLOCATION_GRANULARITY, PAGE_SIZE};

use crate::api::*;
use crate::heap::{Heap, Realloc};
use crate::thread::{Halt, Thread};

const MEM_COMMIT: u32 = 0x1000;
const MEM_RESERVE: u32 = 0x2000;
const MEM_DECOMMIT: u32 = 0x4000;
const MEM_RELEASE: u32 = 0x8000;
const MEM_FREE: u32 = 0x1_0000;
const MEM_PRIVATE: u32 = 0x2_0000;
const MEM_RESET: u32 = 0x8_0000;
const MEM_IMAGE: u32 = 0x100_0000;

pub(crate) const PAGE_NOACCESS: u32 = 0x01;
pub(crate) const PAGE_READONLY: u32 = 0x02;
pub(crate) const PAGE_READWRITE: u32 = 0x04;
const PAGE_WRITECOPY: u32 = 0x08;
const PAGE_EXECUTE: u32 = 0x10;
const PAGE_EXECUTE_READ: u32 = 0x20;
const PAGE_EXECUTE_READWRITE: u32 = 0x40;
const PAGE_EXECUTE_WRITECOPY: u32 = 0x80;
/// Guard, no-cache and write-combine modifiers, which AnyOS ignores.
const PAGE_MODIFIERS: u32 = 0x700;

const HEAP_GENERATE_EXCEPTIONS: u32 = 0x4;
const HEAP_ZERO_MEMORY: u32 = 0x8;
const HEAP_REALLOC_IN_PLACE_ONLY: u32 = 0x10;
const LMEM_MOVEABLE: u32 = 0x2;
const LMEM_ZEROINIT: u32 = 0x40;
const LMEM_MODIFY: u32 = 0x80;

/// A `VirtualAlloc` reservation.
#[derive(Debug, Clone)]
pub(crate) struct Reservation {
    pub size: u64,
    pub alloc_protect: u32,
    /// Per page: the protection code it was committed with, 0 when reserved only.
    pub pages: Vec<u32>,
}

pub(crate) fn to_protection(p: u32) -> Option<Protection> {
    Some(match p & !PAGE_MODIFIERS {
        PAGE_NOACCESS => Protection::NONE,
        PAGE_READONLY => Protection::R,
        PAGE_READWRITE | PAGE_WRITECOPY => Protection::RW,
        PAGE_EXECUTE => Protection::X,
        PAGE_EXECUTE_READ => Protection::RX,
        PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY => Protection::RWX,
        _ => return None,
    })
}

pub(crate) fn from_protection(p: Protection) -> u32 {
    match (p.read, p.write, p.execute) {
        (_, true, true) => PAGE_EXECUTE_READWRITE,
        (_, false, true) if p.read => PAGE_EXECUTE_READ,
        (_, false, true) => PAGE_EXECUTE,
        (_, true, false) => PAGE_READWRITE,
        (true, false, false) => PAGE_READONLY,
        _ => PAGE_NOACCESS,
    }
}

/// The reservation containing `addr`, if any: (base, reservation).
fn reservation_at<'t>(t: &'t Thread<'_, '_>, addr: u64) -> Option<(u64, &'t Reservation)> {
    t.k.reservations
        .range(..=addr)
        .next_back()
        .filter(|(&b, r)| addr < b + r.size)
        .map(|(&b, r)| (b, r))
}

fn virtual_alloc(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (addr, size, ty, protect) = (a[0], a[1], a[2] as u32, a[3] as u32);
    let Some(prot) = to_protection(protect) else {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    };
    if size == 0 {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    }
    if ty & MEM_RESET != 0 {
        return Ok(round_down(addr, PAGE_SIZE));
    }
    if ty & MEM_RESERVE != 0 || addr == 0 {
        if ty & (MEM_RESERVE | MEM_COMMIT) == 0 {
            return fail(t, ERROR_INVALID_PARAMETER, 0);
        }
        let commit = ty & MEM_COMMIT != 0;
        let initial = if commit { prot } else { Protection::NONE };
        let (base, len) = if addr == 0 {
            let len = round_up(size, PAGE_SIZE);
            match t
                .process
                .memory
                .map_anywhere(len, initial, "VirtualAlloc", None)
            {
                Ok(b) => (b, len),
                Err(_) => return fail(t, ERROR_NOT_ENOUGH_MEMORY, 0),
            }
        } else {
            let base = round_down(addr, ALLOCATION_GRANULARITY);
            let len = round_up(addr + size, PAGE_SIZE) - base;
            if t.process
                .memory
                .map(base, len, initial, "VirtualAlloc")
                .is_err()
            {
                return fail(t, ERROR_INVALID_ADDRESS, 0);
            }
            (base, len)
        };
        let pages = (len / PAGE_SIZE) as usize;
        t.k.reservations.insert(
            base,
            Reservation {
                size: len,
                alloc_protect: protect,
                pages: vec![if commit { protect } else { 0 }; pages],
            },
        );
        return Ok(base);
    }
    // Commit pages of an existing reservation.
    let first = round_down(addr, PAGE_SIZE);
    let last = round_up(addr + size, PAGE_SIZE);
    let Some((base, r)) = reservation_at(t, first) else {
        return fail(t, ERROR_INVALID_ADDRESS, 0);
    };
    if last > base + r.size {
        return fail(t, ERROR_INVALID_ADDRESS, 0);
    }
    if t.process.memory.protect(first, last - first, prot).is_err() {
        return fail(t, ERROR_INVALID_ADDRESS, 0);
    }
    let r = t.k.reservations.get_mut(&base).expect("found above");
    let (p0, p1) = (
        ((first - base) / PAGE_SIZE) as usize,
        ((last - base) / PAGE_SIZE) as usize,
    );
    r.pages[p0..p1].fill(protect);
    Ok(first)
}

fn virtual_free(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (addr, size, ty) = (a[0], a[1], a[2] as u32);
    if ty == MEM_RELEASE {
        if size != 0 || !t.k.reservations.contains_key(&addr) {
            return fail(t, ERROR_INVALID_PARAMETER, 0);
        }
        t.k.reservations.remove(&addr);
        let _ = t.process.memory.unmap(addr);
        return Ok(1);
    }
    if ty != MEM_DECOMMIT {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    }
    let Some((base, r)) = reservation_at(t, addr) else {
        return fail(t, ERROR_INVALID_ADDRESS, 0);
    };
    let (first, last) = if size == 0 {
        (base, base + r.size)
    } else {
        (
            round_down(addr, PAGE_SIZE),
            round_up(addr + size, PAGE_SIZE),
        )
    };
    if last > base + r.size {
        return fail(t, ERROR_INVALID_ADDRESS, 0);
    }
    // Decommitted pages read as zero when committed again.
    let _ = t
        .process
        .memory
        .write_raw(first, &vec![0; (last - first) as usize]);
    let _ = t
        .process
        .memory
        .protect(first, last - first, Protection::NONE);
    let r = t.k.reservations.get_mut(&base).expect("found above");
    let (p0, p1) = (
        ((first - base) / PAGE_SIZE) as usize,
        ((last - base) / PAGE_SIZE) as usize,
    );
    r.pages[p0..p1].fill(0);
    Ok(1)
}

fn virtual_protect(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (addr, size, protect, old_ptr) = (a[0], a[1], a[2] as u32, a[3]);
    let Some(prot) = to_protection(protect) else {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    };
    if old_ptr == 0 {
        return fail(t, ERROR_NOACCESS, 0);
    }
    let first = round_down(addr, PAGE_SIZE);
    let last = round_up(addr + size.max(1), PAGE_SIZE);
    let old = if let Some((base, r)) = reservation_at(t, first) {
        let (p0, p1) = (
            ((first - base) / PAGE_SIZE) as usize,
            ((last - base) / PAGE_SIZE) as usize,
        );
        if last > base + r.size || r.pages[p0..p1].contains(&0) {
            return fail(t, ERROR_INVALID_ADDRESS, 0);
        }
        let old = r.pages[p0];
        t.k.reservations.get_mut(&base).expect("found").pages[p0..p1].fill(protect);
        let _ = t.process.memory.protect(first, last - first, prot);
        old
    } else {
        match t.process.memory.protect(first, last - first, prot) {
            Ok(p) => from_protection(p),
            Err(_) => return fail(t, ERROR_INVALID_ADDRESS, 0),
        }
    };
    if !write_u32(t, old_ptr, old) {
        return fail(t, ERROR_NOACCESS, 0);
    }
    Ok(1)
}

/// What `VirtualQuery` reports for `addr`.
struct Mbi {
    base: u64,
    alloc_base: u64,
    alloc_protect: u32,
    size: u64,
    state: u32,
    protect: u32,
    ty: u32,
}

fn query(t: &Thread<'_, '_>, addr: u64) -> Option<Mbi> {
    let page = round_down(addr, PAGE_SIZE);
    let (low, high) = t.process.memory.bounds();
    if page >= high {
        return None;
    }
    if let Some((base, r)) = reservation_at(t, page) {
        let i = ((page - base) / PAGE_SIZE) as usize;
        let code = r.pages[i];
        let run = r.pages[i..].iter().take_while(|&&c| c == code).count() as u64;
        return Some(Mbi {
            base: page,
            alloc_base: base,
            alloc_protect: r.alloc_protect,
            size: run * PAGE_SIZE,
            state: if code == 0 { MEM_RESERVE } else { MEM_COMMIT },
            protect: code,
            ty: MEM_PRIVATE,
        });
    }
    if let Some(info) = t.process.memory.query(page) {
        let off = page - info.base;
        let (run_off, run_len, prot) = info
            .protections
            .iter()
            .copied()
            .find(|&(o, l, _)| off >= o && off < o + l)?;
        let module = t
            .process
            .modules
            .iter()
            .find(|m| page >= m.base && page < m.base + m.size);
        return Some(Mbi {
            base: page,
            alloc_base: module.map_or(info.base, |m| m.base),
            alloc_protect: if module.is_some() {
                PAGE_EXECUTE_WRITECOPY
            } else {
                PAGE_READWRITE
            },
            size: run_off + run_len - off,
            state: MEM_COMMIT,
            protect: from_protection(prot),
            ty: if module.is_some() {
                MEM_IMAGE
            } else {
                MEM_PRIVATE
            },
        });
    }
    // Free: up to the next mapping.
    let next = t
        .process
        .memory
        .regions()
        .iter()
        .map(|r| r.base)
        .filter(|&b| b > page)
        .min()
        .unwrap_or(high);
    Some(Mbi {
        base: page.max(low.min(page)),
        alloc_base: 0,
        alloc_protect: 0,
        size: next - page,
        state: MEM_FREE,
        protect: PAGE_NOACCESS,
        ty: 0,
    })
}

fn virtual_query(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (addr, buf, len) = (a[0], a[1], a[2]);
    let need = if t.long_mode() { 48 } else { 28 };
    if len < need {
        return fail(t, ERROR_INSUFFICIENT_BUFFER, 0);
    }
    let Some(m) = query(t, addr) else {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    };
    let mut out = Vec::new();
    let ptr = |out: &mut Vec<u8>, v: u64, long: bool| {
        if long {
            out.extend_from_slice(&v.to_le_bytes());
        } else {
            out.extend_from_slice(&(v as u32).to_le_bytes());
        }
    };
    let long = t.long_mode();
    ptr(&mut out, m.base, long);
    ptr(&mut out, m.alloc_base, long);
    out.extend_from_slice(&m.alloc_protect.to_le_bytes());
    if long {
        out.extend_from_slice(&[0; 4]); // PartitionId and padding
    }
    ptr(&mut out, m.size, long);
    for v in [m.state, m.protect, m.ty] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    if long {
        out.extend_from_slice(&[0; 4]);
    }
    if !write_bytes(t, buf, &out) {
        return fail(t, ERROR_NOACCESS, 0);
    }
    Ok(need)
}

/// Whether `len` bytes at `addr` are all readable (or writable).
fn accessible(t: &Thread<'_, '_>, addr: u64, len: u64, write: bool) -> bool {
    if len == 0 {
        return true;
    }
    let mut p = round_down(addr, PAGE_SIZE);
    while p < addr.saturating_add(len) {
        match t.process.memory.protection_at(p) {
            Ok(pr) if if write { pr.write } else { pr.read } => {}
            _ => return false,
        }
        p += PAGE_SIZE;
    }
    true
}

/// Copy guest memory for Read/WriteProcessMemory on the running process.
fn copy_process_memory(t: &mut Thread<'_, '_>, a: &[u64], write: bool) -> Result<u64, Halt> {
    let (h, addr, buf, len, done) = (a[0], a[1], a[2], a[3], a[4]);
    if h as u32 != 0xFFFF_FFFF && !matches!(t.k.get(h), Some(crate::objects::Object::SelfProcess)) {
        return fail(t, ERROR_ACCESS_DENIED, 0);
    }
    let (from, to) = if write { (buf, addr) } else { (addr, buf) };
    let Some(data) = guest_bytes(t, from, len) else {
        store_ptr(t, done, 0);
        return fail(t, 299, 0); // ERROR_PARTIAL_COPY
    };
    let ok = if write {
        // Writing code is allowed even into read-only pages, like a debugger.
        t.process.memory.write_raw(to, &data).is_ok()
    } else {
        write_bytes(t, to, &data)
    };
    if !ok {
        store_ptr(t, done, 0);
        return fail(t, 299, 0);
    }
    store_ptr(t, done, len);
    Ok(1)
}

// ---------------------------------------------------------------------------
// Heaps

/// Create a heap and return its handle.
pub(crate) fn create_heap(t: &mut Thread<'_, '_>) -> Result<u64, Halt> {
    let len = Heap::segment_size(0);
    let base = t
        .process
        .memory
        .map_anywhere(len, Protection::RW, "heap", None)?;
    t.k.heaps.push(Heap::new(base, len));
    Ok(base)
}

/// Allocate from the heap with handle `h`. Returns 0 on failure.
pub(crate) fn heap_alloc(t: &mut Thread<'_, '_>, h: u64, size: u64, zero: bool) -> u64 {
    let mem = &mut t.process.memory;
    let Some(heap) = t.k.heaps.iter_mut().find(|x| x.handle == h) else {
        return 0;
    };
    let got = heap.alloc(size, |len| {
        mem.map_anywhere(len, Protection::RW, "heap", None).ok()
    });
    match got {
        Some((p, fresh)) => {
            if zero && !fresh {
                let _ = mem.write_raw(p, &vec![0; size as usize]);
            }
            p
        }
        None => 0,
    }
}

/// Free a heap block, whichever heap owns it.
pub(crate) fn heap_free(t: &mut Thread<'_, '_>, h: u64, p: u64) -> bool {
    let owner =
        t.k.heaps
            .iter()
            .position(|x| x.handle == h && x.owns(p))
            .or_else(|| t.k.heaps.iter().position(|x| x.owns(p)));
    let Some(i) = owner else {
        return false;
    };
    match t.k.heaps[i].free(p) {
        Ok(Some(mapping)) => {
            let _ = t.process.memory.unmap(mapping);
            true
        }
        Ok(None) => true,
        Err(()) => false,
    }
}

fn heap_size_of(t: &Thread<'_, '_>, p: u64) -> Option<u64> {
    t.k.heaps.iter().find_map(|h| h.size(p))
}

/// Resize a heap block. Returns the (possibly new) address, or 0.
pub(crate) fn heap_realloc(t: &mut Thread<'_, '_>, p: u64, size: u64, flags: u32) -> u64 {
    let Some(i) = t.k.heaps.iter().position(|x| x.owns(p)) else {
        return 0;
    };
    let old = t.k.heaps[i].size(p).unwrap_or(0);
    let mem = &mut t.process.memory;
    let r = t.k.heaps[i].realloc(p, size, flags & HEAP_REALLOC_IN_PLACE_ONLY != 0, |len| {
        mem.map_anywhere(len, Protection::RW, "heap", None).ok()
    });
    let new = match r {
        None => return 0,
        Some(Realloc::InPlace) => p,
        Some(Realloc::Moved { new, copy }) => {
            let mut data = vec![0u8; copy as usize];
            let _ = mem.read_raw(p, &mut data);
            let _ = mem.write_raw(new, &data);
            if let Ok(Some(m)) = t.k.heaps[i].free(p) {
                let _ = t.process.memory.unmap(m);
            }
            new
        }
    };
    if flags & HEAP_ZERO_MEMORY != 0 && size > old {
        let _ = t
            .process
            .memory
            .write_raw(new + old, &vec![0; (size - old) as usize]);
    }
    new
}

fn heap_handle(t: &Thread<'_, '_>, h: u64) -> bool {
    t.k.heaps.iter().any(|x| x.handle == h)
}

fn heap_create(t: &mut Thread<'_, '_>, _: &[u64]) -> Result<u64, Halt> {
    create_heap(t)
}

fn heap_destroy(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let h = a[0];
    if h == t.k.process_heap {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    }
    let Some(i) = t.k.heaps.iter().position(|x| x.handle == h) else {
        return fail(t, ERROR_INVALID_HANDLE, 0);
    };
    let heap = t.k.heaps.remove(i);
    for m in heap.mappings() {
        let _ = t.process.memory.unmap(m);
    }
    Ok(1)
}

fn heap_alloc_api(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (h, flags, size) = (a[0], a[1] as u32, a[2]);
    if !heap_handle(t, h) {
        return fail(t, ERROR_INVALID_HANDLE, 0);
    }
    let p = heap_alloc(t, h, size, flags & HEAP_ZERO_MEMORY != 0);
    if p == 0 {
        if flags & HEAP_GENERATE_EXCEPTIONS != 0 {
            t.trace_note("HeapAlloc failed (would raise STATUS_NO_MEMORY)");
        }
        t.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
    }
    Ok(p)
}

fn heap_free_api(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (h, p) = (a[0], a[2]);
    if p == 0 {
        return Ok(1);
    }
    if heap_free(t, h, p) {
        Ok(1)
    } else {
        fail(t, ERROR_INVALID_PARAMETER, 0)
    }
}

fn heap_realloc_api(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (flags, p, size) = (a[1] as u32, a[2], a[3]);
    if p == 0 {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    }
    let new = heap_realloc(t, p, size, flags);
    if new == 0 {
        t.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
    }
    Ok(new)
}

fn heap_size_api(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    match heap_size_of(t, a[2]) {
        Some(s) => Ok(s),
        None => {
            t.set_last_error(ERROR_INVALID_PARAMETER);
            Ok(if t.long_mode() { u64::MAX } else { 0xFFFF_FFFF })
        }
    }
}

fn get_process_heaps(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (n, buf) = (a[0] as usize, a[1]);
    let handles: Vec<u64> = t.k.heaps.iter().map(|h| h.handle).collect();
    if n >= handles.len() {
        let p = t.ptr_size() as u64;
        for (i, h) in handles.iter().enumerate() {
            write_ptr(t, buf + i as u64 * p, *h);
        }
    }
    Ok(handles.len() as u64)
}

// Local and Global memory: the same on 32- and 64-bit Windows.

fn local_alloc(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (flags, size) = (a[0] as u32, a[1]);
    let h = t.k.process_heap;
    let p = heap_alloc(t, h, size, flags & LMEM_ZEROINIT != 0);
    if p == 0 {
        t.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
    }
    Ok(p)
}

fn local_free(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let p = a[0];
    let h = t.k.process_heap;
    if p == 0 || heap_free(t, h, p) {
        Ok(0)
    } else {
        fail(t, ERROR_INVALID_HANDLE, p)
    }
}

fn local_realloc(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (p, size, flags) = (a[0], a[1], a[2] as u32);
    if flags & LMEM_MODIFY != 0 {
        return Ok(p);
    }
    let mut hf = if flags & LMEM_ZEROINIT != 0 {
        HEAP_ZERO_MEMORY
    } else {
        0
    };
    if flags & LMEM_MOVEABLE == 0 {
        hf |= HEAP_REALLOC_IN_PLACE_ONLY;
    }
    let new = heap_realloc(t, p, size, hf);
    if new == 0 {
        t.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
    }
    Ok(new)
}

fn local_size(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    match heap_size_of(t, a[0]) {
        Some(s) => Ok(s),
        None => fail(t, ERROR_INVALID_HANDLE, 0),
    }
}

fn global_unlock(t: &mut Thread<'_, '_>, _: &[u64]) -> Result<u64, Halt> {
    t.set_last_error(ERROR_SUCCESS);
    Ok(0)
}

pub(crate) static APIS: &[Api] = &[
    k32("VirtualAlloc", 4, virtual_alloc),
    k32("VirtualFree", 3, virtual_free),
    k32("VirtualProtect", 4, virtual_protect),
    k32("VirtualQuery", 3, virtual_query),
    k32("VirtualAllocEx", 5, |t, a| virtual_alloc(t, &a[1..])),
    k32("VirtualFreeEx", 4, |t, a| virtual_free(t, &a[1..])),
    k32("VirtualProtectEx", 5, |t, a| virtual_protect(t, &a[1..])),
    k32("VirtualQueryEx", 4, |t, a| virtual_query(t, &a[1..])),
    k32("VirtualLock", 2, |_, _| Ok(1)),
    k32("VirtualUnlock", 2, |_, _| Ok(1)),
    k32("FlushInstructionCache", 3, |_, _| Ok(1)),
    k32("ReadProcessMemory", 5, |t, a| {
        copy_process_memory(t, a, false)
    }),
    k32("WriteProcessMemory", 5, |t, a| {
        copy_process_memory(t, a, true)
    }),
    k32("IsBadReadPtr", 2, |t, a| {
        Ok(!accessible(t, a[0], a[1], false) as u64)
    }),
    k32("IsBadWritePtr", 2, |t, a| {
        Ok(!accessible(t, a[0], a[1], true) as u64)
    }),
    k32("IsBadCodePtr", 1, |t, a| {
        Ok(!accessible(t, a[0], 1, false) as u64)
    }),
    k32("IsBadStringPtrA", 2, |t, a| {
        Ok(t.process
            .memory
            .read_cstr(a[0], a[1].min(1 << 24) as usize)
            .is_err() as u64)
    }),
    k32("IsBadStringPtrW", 2, |t, a| {
        Ok((a[0] == 0 || read_u16(t, a[0]).is_none()) as u64)
    }),
    k32("GetProcessHeap", 0, |t, _| Ok(t.k.process_heap)),
    k32("GetProcessHeaps", 2, get_process_heaps),
    k32("HeapCreate", 3, heap_create),
    k32("HeapDestroy", 1, heap_destroy),
    k32("HeapAlloc", 3, heap_alloc_api),
    k32("HeapFree", 3, heap_free_api),
    k32("HeapReAlloc", 4, heap_realloc_api),
    k32("HeapSize", 3, heap_size_api),
    k32("HeapValidate", 3, |_, _| Ok(1)),
    k32("HeapCompact", 2, |t, a| {
        Ok(t.k
            .heaps
            .iter()
            .find(|h| h.handle == a[0])
            .map_or(0, Heap::largest_free))
    }),
    k32("HeapLock", 1, |_, _| Ok(1)),
    k32("HeapUnlock", 1, |_, _| Ok(1)),
    k32("HeapWalk", 2, |t, _| fail(t, ERROR_NO_MORE_ITEMS, 0)),
    k32("HeapSetInformation", 4, |_, _| Ok(1)),
    k32("HeapQueryInformation", 5, |t, a| {
        // HeapCompatibilityInformation: 2, the low-fragmentation heap.
        if a[1] != 0 || a[3] < 4 {
            return fail(t, ERROR_INSUFFICIENT_BUFFER, 0);
        }
        write_u32(t, a[2], 2);
        store_ptr(t, a[4], 4);
        Ok(1)
    }),
    k32("LocalAlloc", 2, local_alloc),
    k32("LocalFree", 1, local_free),
    k32("LocalReAlloc", 3, local_realloc),
    k32("LocalSize", 1, local_size),
    k32("LocalLock", 1, |_, a| Ok(a[0])),
    k32("LocalUnlock", 1, global_unlock),
    k32("LocalHandle", 1, |_, a| Ok(a[0])),
    k32("LocalFlags", 1, |_, _| Ok(0)),
    k32("LocalCompact", 1, |_, _| Ok(0x10_0000)),
    k32("LocalShrink", 2, |_, _| Ok(0x10_0000)),
    k32("GlobalAlloc", 2, local_alloc),
    k32("GlobalFree", 1, local_free),
    k32("GlobalReAlloc", 3, local_realloc),
    k32("GlobalSize", 1, local_size),
    k32("GlobalLock", 1, |_, a| Ok(a[0])),
    k32("GlobalUnlock", 1, global_unlock),
    k32("GlobalHandle", 1, |_, a| Ok(a[0])),
    k32("GlobalFlags", 1, |_, _| Ok(0)),
    k32("GlobalCompact", 1, |_, _| Ok(0x10_0000)),
    k32("GlobalWire", 1, |_, a| Ok(a[0])),
    k32("GlobalUnWire", 1, |_, _| Ok(1)),
    k32("GlobalFix", 1, |_, _| Ok(0)),
    k32("GlobalUnfix", 1, |_, _| Ok(0)),
];
