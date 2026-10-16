//! The API call gate: where imports that AnyOS itself must provide are bound.
//!
//! Every Windows API the guest imports from a system DLL gets a unique stub
//! address inside a dedicated, execute-only region. The stub bytes are an
//! architecture-appropriate trap (`int3` on x86, `brk` on ARM64), so a guest
//! that reaches one stops in a controlled way. The execution backend (next
//! milestone) recognises a stop inside the gate region, looks up the stub id,
//! and dispatches to the Rust implementation of that API. This keeps raw host
//! pointers out of guest memory entirely.

use anyos_memory::{AddressSpace, MemoryError, Protection};
use anyos_pe::{ImportName, Machine};
use serde::Serialize;

/// Bytes reserved per stub.
pub const STUB_SIZE: u64 = 16;
const CHUNK_SIZE: u64 = 0x1_0000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GateEntry {
    pub id: u32,
    pub address: u64,
    /// Lower-cased DLL name the import was requested from.
    pub dll: String,
    pub symbol: ImportName,
}

#[derive(Debug, Default)]
pub struct CallGate {
    chunks: Vec<u64>,
    entries: Vec<GateEntry>,
    index: std::collections::HashMap<(String, ImportName), u32>,
}

impl CallGate {
    pub fn entries(&self) -> &[GateEntry] {
        &self.entries
    }

    /// The stub for `(dll, symbol)`, creating it on first use.
    pub fn stub_for(
        &mut self,
        mem: &mut AddressSpace,
        machine: Machine,
        dll: &str,
        symbol: &ImportName,
    ) -> Result<u64, MemoryError> {
        let key = (dll.to_ascii_lowercase(), symbol.clone());
        if let Some(&id) = self.index.get(&key) {
            return Ok(self.entries[id as usize].address);
        }
        let id = self.entries.len() as u32;
        let per_chunk = CHUNK_SIZE / STUB_SIZE;
        if (id as u64).is_multiple_of(per_chunk) {
            let base = mem.map_anywhere(CHUNK_SIZE, Protection::RW, "anyos call gate", None)?;
            self.chunks.push(base);
            fill_traps(mem, base, machine)?;
            mem.protect(base, CHUNK_SIZE, Protection::X)?;
        }
        let address =
            self.chunks[(id as u64 / per_chunk) as usize] + (id as u64 % per_chunk) * STUB_SIZE;
        self.entries.push(GateEntry {
            id,
            address,
            dll: key.0.clone(),
            symbol: key.1.clone(),
        });
        self.index.insert(key, id);
        Ok(address)
    }

    /// Which stub starts at `address`, if any.
    pub fn lookup(&self, address: u64) -> Option<&GateEntry> {
        let (entry, offset) = self.owner(address)?;
        (offset == 0).then_some(entry)
    }

    /// The stub `address` is inside, with its offset into that stub's
    /// [`STUB_SIZE`] bytes. Every byte of a chunk is a trap, so a guest that
    /// lands past the first byte of a stub (a resume just after the trap, see
    /// `anyos-win32`'s SEH dispatch) still belongs to a known API.
    pub fn owner(&self, address: u64) -> Option<(&GateEntry, u64)> {
        let per_chunk = CHUNK_SIZE / STUB_SIZE;
        let (chunk_idx, base) = self
            .chunks
            .iter()
            .enumerate()
            .find(|(_, &b)| address >= b && address < b + CHUNK_SIZE)?;
        let offset = address - base;
        let id = chunk_idx as u64 * per_chunk + offset / STUB_SIZE;
        self.entries
            .get(id as usize)
            .map(|e| (e, offset % STUB_SIZE))
    }
}

fn fill_traps(mem: &mut AddressSpace, base: u64, machine: Machine) -> Result<(), MemoryError> {
    let pattern: &[u8] = match machine {
        // BRK #0xF000 (the Windows ARM64 debug-break immediate).
        Machine::Arm64 | Machine::Arm64Ec => &0xD43E_0000u32.to_le_bytes(),
        // UDF #0xFE (Thumb) for 32-bit ARM.
        Machine::ArmNt => &[0xFE, 0xDE],
        // INT3 for x86 / x86-64 and anything unknown.
        _ => &[0xCC],
    };
    let buf: Vec<u8> = pattern
        .iter()
        .copied()
        .cycle()
        .take(CHUNK_SIZE as usize)
        .collect();
    mem.write_raw(base, &buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(s: &str) -> ImportName {
        ImportName::Name {
            name: s.into(),
            hint: 0,
        }
    }

    #[test]
    fn a_stub_owns_all_of_its_bytes() {
        let mut mem = AddressSpace::new(0x10000, 0x8000_0000, 1 << 24);
        let mut gate = CallGate::default();
        let first = gate
            .stub_for(
                &mut mem,
                Machine::X86,
                "kernel32.dll",
                &name("GetLastError"),
            )
            .unwrap();
        let second = gate
            .stub_for(
                &mut mem,
                Machine::X86,
                "kernel32.dll",
                &name("RaiseException"),
            )
            .unwrap();
        assert_eq!(second, first + STUB_SIZE);
        // Only a stub's first byte is a call site.
        assert_eq!(gate.lookup(first).unwrap().symbol, name("GetLastError"));
        assert!(gate.lookup(first + 1).is_none());
        // The rest of the stub -- all trap bytes -- still resolves to it, which
        // is how a resume just past the trap finds its way home.
        assert_eq!(gate.owner(first + 1).unwrap().1, 1);
        let (entry, offset) = gate.owner(first + STUB_SIZE).unwrap();
        assert_eq!((entry.symbol.clone(), offset), (name("RaiseException"), 0));
        // Outside the gate region there is no owner.
        assert!(gate.owner(0x20000).is_none());
    }
}
