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
        if id as u64 % per_chunk == 0 {
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

    /// Which stub, if any, lives at `address`.
    pub fn lookup(&self, address: u64) -> Option<&GateEntry> {
        let per_chunk = CHUNK_SIZE / STUB_SIZE;
        let (chunk_idx, base) = self
            .chunks
            .iter()
            .enumerate()
            .find(|(_, &b)| address >= b && address < b + CHUNK_SIZE)?;
        if (address - base) % STUB_SIZE != 0 {
            return None;
        }
        let id = chunk_idx as u64 * per_chunk + (address - base) / STUB_SIZE;
        self.entries.get(id as usize)
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
