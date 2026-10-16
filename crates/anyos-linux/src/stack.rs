//! Initial stack layout for Linux x86-64 programs.
//!
//! When the kernel hands control to `_start`, the stack looks like:
//!
//! ```text
//! [high]
//! (strings: argv[0..n], envp[0..m], zero bytes)
//!  ...
//! AT_NULL (2 × u64 = 0, 0)          ← auxv terminator
//! AT_ENTRY u64                        ← entry point
//! AT_PHNUM u64                        ← number of program headers
//! AT_PHENT u64                        ← size of one PH entry
//! AT_PHDR  u64                        ← vaddr of the phdr table
//! AT_PAGESZ u64 = 4096
//! 0 (envp terminator)
//! envp[m-1] *char
//! ...
//! envp[0] *char
//! 0 (argv terminator)
//! argv[n-1] *char
//! ...
//! argv[0] *char
//! argc u64
//! [rsp here]
//! [low]
//! ```

use anyos_memory::{AddressSpace, Protection};

/// Parameters for constructing the initial stack.
pub struct StackParams<'a> {
    /// Guest virtual address of the top of the stack region (exclusive bound).
    pub stack_top: u64,
    /// Size in bytes to reserve for the stack.
    pub stack_size: u64,
    /// Program arguments (argv[0] should be the program path or name).
    pub argv: &'a [&'a str],
    /// Environment variables as "KEY=VALUE" strings.
    pub envp: &'a [&'a str],
    /// AT_PHDR from the loaded ELF image.
    pub at_phdr: u64,
    /// AT_PHNUM from the loaded ELF image.
    pub at_phnum: u64,
    /// AT_PHENT from the loaded ELF image.
    pub at_phent: u64,
    /// AT_ENTRY from the loaded ELF image.
    pub at_entry: u64,
    /// AT_BASE: load base of the dynamic interpreter (0 for static binaries).
    pub at_base: u64,
    /// 16 bytes of random data for AT_RANDOM (stack canary seed etc.).
    pub at_random: [u8; 16],
}

/// Write the initial stack into `space` and return the value that `rsp` should
/// hold when control is transferred to the guest entry point.
pub fn build(
    space: &mut AddressSpace,
    p: &StackParams<'_>,
) -> Result<u64, anyos_memory::MemoryError> {
    let stack_bottom = p.stack_top - p.stack_size;
    space.map(stack_bottom, p.stack_size, Protection::RW, "stack")?;

    // We write the layout bottom-up from stack_top, then flip the pointer
    // array section (which must appear top-down in guest memory).
    // Use a local buffer; copy it to guest memory at the end.

    // ── string data area (grows downward from stack_top) ─────────────────
    let mut strings: Vec<u8> = Vec::new();

    fn push_str(strings: &mut Vec<u8>, s: &str) -> usize {
        let off = strings.len();
        strings.extend_from_slice(s.as_bytes());
        strings.push(0);
        off
    }

    let argv_offsets: Vec<usize> = p.argv.iter().map(|s| push_str(&mut strings, s)).collect();
    let envp_offsets: Vec<usize> = p.envp.iter().map(|s| push_str(&mut strings, s)).collect();

    // AT_RANDOM: 16 bytes placed in the string area (just a fixed buffer here).
    let at_random_off = strings.len();
    strings.extend_from_slice(&p.at_random);

    // Align the strings region to 16 bytes.
    while !strings.len().is_multiple_of(16) {
        strings.push(0);
    }

    // String data sits just below stack_top.
    let string_vaddr = p.stack_top - strings.len() as u64;
    space.write_raw(string_vaddr, &strings)?;

    let at_random_vaddr = string_vaddr + at_random_off as u64;

    // ── pointer / integer area (grows downward from string_vaddr) ─────────
    // We build the words in a Vec<u64> (low word = low address) and write them.
    let mut words: Vec<u64> = Vec::new();

    // auxv (terminator last, so reversed; we'll reverse the whole block later).
    // AT_BASE = 0 means no interpreter (static); non-zero = interpreter load base.
    let mut auxv: Vec<(u64, u64)> = vec![
        (3, p.at_phdr),
        (4, p.at_phent),
        (5, p.at_phnum),
        (6, 4096),             // AT_PAGESZ
        (9, p.at_entry),       // AT_ENTRY
        (25, at_random_vaddr), // AT_RANDOM
    ];
    if p.at_base != 0 {
        auxv.push((7, p.at_base)); // AT_BASE: interpreter load address
    }
    auxv.push((0, 0)); // AT_NULL terminator
    for &(ty, val) in auxv.iter().rev() {
        words.push(val);
        words.push(ty);
    }

    // envp[]: null-terminated.
    words.push(0);
    for &off in envp_offsets.iter().rev() {
        words.push(string_vaddr + off as u64);
    }
    // argv[]: null-terminated.
    words.push(0);
    for &off in argv_offsets.iter().rev() {
        words.push(string_vaddr + off as u64);
    }
    // argc
    words.push(p.argv.len() as u64);

    // The words are now in reverse order: argc is the last element but should
    // be at the lowest address. Reverse so that words[0] = argc.
    words.reverse();

    let words_size = words.len() as u64 * 8;
    let rsp_vaddr = string_vaddr - words_size;

    // Align rsp to 16 bytes (ABI requirement).
    let rsp_aligned = rsp_vaddr & !15;
    let pad = rsp_vaddr - rsp_aligned;
    let rsp_final = if pad > 0 { rsp_aligned } else { rsp_vaddr };

    // Write words to guest memory.
    let mut buf: Vec<u8> = Vec::with_capacity(words.len() * 8);
    for w in &words {
        buf.extend_from_slice(&w.to_le_bytes());
    }
    space.write_raw(rsp_final + pad, &buf)?;

    Ok(rsp_final)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyos_memory::AddressSpace;

    fn make_space() -> AddressSpace {
        AddressSpace::new(0x1_0000, 0x7fff_ffff_f000, 64 << 20)
    }

    #[test]
    fn stack_builds_and_argc_correct() {
        let mut space = make_space();
        let p = StackParams {
            stack_top: 0x7fff_0000,
            stack_size: 0x10000,
            argv: &["hello"],
            envp: &[],
            at_phdr: 0x400040,
            at_phnum: 8,
            at_phent: 56,
            at_entry: 0x401000,
            at_base: 0,
            at_random: [0u8; 16],
        };
        let rsp = build(&mut space, &p).expect("build stack");
        // argc = 1
        let argc = space.read_u64_raw(rsp).expect("argc");
        assert_eq!(argc, 1);
    }
}
