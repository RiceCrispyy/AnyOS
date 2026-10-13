use anyos_elf::{load, summarise, ElfArch, Linkage};
use anyos_memory::AddressSpace;

fn linux_space() -> AddressSpace {
    // Typical 64-bit Linux user space: 0x10000 .. 0x7ffffffff000.
    AddressSpace::new(0x1_0000, 0x7fff_ffff_f000, 256 << 20)
}

fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/bin/other")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("fixture {name}: {e}"))
}

#[test]
fn static_x64_summarise() {
    let bytes = fixture("linux-x64-static");
    let s = summarise(&bytes).expect("summarise static ELF");
    assert_eq!(s.arch, ElfArch::X86_64);
    assert!(!s.is_pie, "static musl build should be ET_EXEC");
    assert!(
        matches!(s.linkage, Linkage::Static),
        "expected static linkage"
    );
}

#[test]
fn dynamic_x64_summarise() {
    let bytes = fixture("linux-x64");
    let s = summarise(&bytes).expect("summarise dynamic ELF");
    assert_eq!(s.arch, ElfArch::X86_64);
    match &s.linkage {
        Linkage::Dynamic { interpreter } => {
            assert!(interpreter.contains("ld-linux"), "unexpected interp: {interpreter}");
        }
        _ => panic!("expected dynamic linkage"),
    }
}

#[test]
fn static_x64_load() {
    let bytes = fixture("linux-x64-static");
    let mut space = linux_space();
    let loaded = load(&mut space, &bytes).expect("load static ELF");
    assert_eq!(loaded.arch, ElfArch::X86_64);
    assert!(!loaded.is_pie);
    assert!(loaded.entry != 0, "entry point should be non-zero");
    // Entry point must be in executable memory.
    space
        .check_execute(loaded.entry)
        .expect("entry point is not executable");
    // PHDR vaddr should also be mapped (readable).
    let mut tmp = [0u8; 1];
    space
        .read(loaded.phdr_vaddr, &mut tmp)
        .expect("PHDR vaddr should be readable");
}

#[test]
fn static_arm64_load() {
    let bytes = fixture("linux-arm64");
    let mut space = linux_space();
    let loaded = load(&mut space, &bytes).expect("load arm64 static ELF");
    assert_eq!(loaded.arch, ElfArch::Aarch64);
    space
        .check_execute(loaded.entry)
        .expect("entry not executable");
}

#[test]
fn pie_x64_load() {
    let bytes = fixture("linux-x64");
    let mut space = linux_space();
    let loaded = load(&mut space, &bytes).expect("load dynamic ELF");
    // Even though it is ET_DYN (PIE) the image should have been placed somewhere.
    assert!(loaded.load_base != 0);
    space
        .check_execute(loaded.entry)
        .expect("entry not executable");
}
