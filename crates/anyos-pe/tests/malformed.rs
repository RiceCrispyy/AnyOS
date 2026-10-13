//! Malformed and hostile input must produce errors, never panics or huge allocations.

use std::path::PathBuf;

use anyos_pe::{parse, parse_with_limits, Limits, PeError};

fn fixture(rel: &str) -> Vec<u8> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(rel);
    std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

const FIXTURES: [&str; 13] = [
    "bin/x64/hello_console.exe",
    "bin/x64/hello_gui.exe",
    "bin/x64/exports_dll.dll",
    "bin/x64/uses_dll.exe",
    "bin/arm64/hello_console.exe",
    "bin/arm64/hello_gui.exe",
    "bin/arm64/exports_dll.dll",
    "bin/arm64/uses_dll.exe",
    "bin/x86/hello_console.exe",
    "bin/x86/hello_gui.exe",
    "bin/x86/exports_dll.dll",
    "bin/x86/uses_dll.exe",
    "third_party/System.Runtime.CompilerServices.Unsafe.dll",
];

fn pe_offset(b: &[u8]) -> usize {
    u32::from_le_bytes(b[0x3c..0x40].try_into().unwrap()) as usize
}

/// Offset of data directory `i` in the optional header.
fn data_dir_offset(b: &[u8], i: usize) -> usize {
    let opt = pe_offset(b) + 24;
    let magic = u16::from_le_bytes([b[opt], b[opt + 1]]);
    opt + if magic == 0x20b { 112 } else { 96 } + i * 8
}

fn put_u32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

#[test]
fn non_pe_input_is_rejected() {
    assert!(matches!(parse(b""), Err(PeError::NotPe(_))));
    assert!(matches!(
        parse(b"\x7fELF\x02\x01\x01"),
        Err(PeError::NotPe(_))
    ));
    assert!(parse(b"MZ").is_err());
    assert!(parse(&[b'M', b'Z', 0, 0, 0, 0, 0, 0]).is_err());
}

#[test]
fn every_truncation_fails_cleanly() {
    for f in FIXTURES {
        let bytes = fixture(f);
        let step = if bytes.len() > 4096 { 7 } else { 1 };
        for len in (0..bytes.len()).step_by(step) {
            // Must return (Ok or Err), not panic. Tail truncation can still parse
            // when only padding was cut, so either outcome is acceptable.
            let _ = parse(&bytes[..len]);
        }
    }
}

#[test]
fn random_mutations_never_panic() {
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut parsed = 0;
    for f in FIXTURES {
        let original = fixture(f);
        for _ in 0..1500 {
            let mut b = original.clone();
            for _ in 0..(1 + next() % 8) {
                let i = (next() % b.len() as u64) as usize;
                b[i] = next() as u8;
            }
            if parse(&b).is_ok() {
                parsed += 1;
            }
        }
    }
    assert!(parsed > 0, "some mutants should still parse");
}

#[test]
fn bad_header_fields_are_reported() {
    let good = fixture("bin/x64/hello_console.exe");

    let mut b = good.clone();
    let at = pe_offset(&b);
    put_u32(&mut b, 0x3c, 0x7fff_0000); // e_lfanew far past the end
    assert!(parse(&b).is_err());

    b = good.clone();
    b[at + 6..at + 8].copy_from_slice(&500u16.to_le_bytes()); // NumberOfSections
    assert!(matches!(
        parse(&b),
        Err(PeError::LimitExceeded(_)) | Err(PeError::Truncated(_)) | Err(PeError::Malformed(_))
    ));

    b = good.clone();
    put_u32(&mut b, at + 24 + 56, 0); // SizeOfImage = 0
    assert!(matches!(parse(&b), Err(PeError::Malformed(_))));

    b = good.clone();
    put_u32(&mut b, at + 24 + 32, 3000); // SectionAlignment not a power of two
    assert!(matches!(parse(&b), Err(PeError::Malformed(_))));

    b = good.clone();
    b[at] = b'X'; // PE signature
    assert!(parse(&b).is_err());
}

#[test]
fn damaged_directories_are_flagged_not_fatal() {
    let good = fixture("bin/x64/hello_console.exe");

    let mut b = good.clone();
    let import_dir = data_dir_offset(&b, 1);
    put_u32(&mut b, import_dir, 0x00ff_0000); // import table RVA outside the image
    let img = parse(&b).unwrap();
    assert_eq!(img.broken_directories, ["import"]);
    assert!(img.imports.is_empty());
    assert!(
        img.sections.len() == 4,
        "the rest of the image is still described"
    );

    b = good.clone();
    let reloc_rva = u32::from_le_bytes(b[data_dir_offset(&b, 5)..][..4].try_into().unwrap());
    let img = parse(&good).unwrap();
    let reloc = img
        .sections
        .iter()
        .find(|s| s.virtual_address == reloc_rva)
        .unwrap();
    let block_size_at = reloc.raw_offset as usize + 4;
    put_u32(&mut b, block_size_at, 3); // relocation block size not >= 8
    let img = parse(&b).unwrap();
    assert_eq!(img.broken_directories, ["base-relocation"]);
}

#[test]
fn limits_are_enforced() {
    let b = fixture("bin/x64/hello_console.exe");
    let tiny = Limits {
        max_relocations: 1,
        ..Limits::default()
    };
    let img = parse_with_limits(&b, &tiny).unwrap();
    assert_eq!(img.broken_directories, ["base-relocation"]);

    let small_file = Limits {
        max_file_size: 100,
        ..Limits::default()
    };
    assert!(matches!(
        parse_with_limits(&b, &small_file),
        Err(PeError::LimitExceeded(_))
    ));

    let one_import = Limits {
        max_imports_per_module: 1,
        ..Limits::default()
    };
    assert_eq!(
        parse_with_limits(&b, &one_import)
            .unwrap()
            .broken_directories,
        ["import"]
    );
}
