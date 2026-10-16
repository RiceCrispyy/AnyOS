//! Loader tests: mapping, relocation, import binding, protections and TLS.

use std::path::{Path, PathBuf};

use anyos_loader::*;
use anyos_memory::{Access, MemoryError, Protection};
use anyos_pe::{ExportTarget, ImportName, PeFormat, RelocationKind};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

const ARCHES: [&str; 3] = ["x64", "arm64", "x86"];

fn load(path: &Path, base: BasePolicy, deps: bool) -> Process {
    let opts = LoadOptions {
        base,
        search_dirs: vec![path.parent().unwrap().to_path_buf()],
        load_dependencies: deps,
    };
    Process::load_executable(path, &opts).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Every absolute fixup must point inside the image, at the same offset from
/// the base whether or not the image was rebased.
fn assert_relocations_consistent(a: &Process, b: &Process) {
    let (ma, mb) = (&a.modules[0], &b.modules[0]);
    assert_ne!(ma.base, mb.base, "test needs two different bases");
    let ptr = a.format.pointer_size();
    for r in &ma.image.relocations {
        let width = match r.kind {
            RelocationKind::Dir64 => 8,
            RelocationKind::HighLow => 4,
            _ => continue,
        };
        assert_eq!(width, ptr);
        let va = a.memory.read_ptr_raw(ma.base + r.rva as u64, ptr).unwrap();
        let vb = b.memory.read_ptr_raw(mb.base + r.rva as u64, ptr).unwrap();
        assert_eq!(
            va.wrapping_sub(ma.base),
            vb.wrapping_sub(mb.base),
            "fixup at RVA 0x{:x}",
            r.rva
        );
    }
}

#[test]
fn maps_at_preferred_base_and_rebases_when_asked() {
    for arch in ARCHES {
        let path = fixtures().join(format!("bin/{arch}/hello_console.exe"));
        let pref = load(&path, BasePolicy::Preferred, false);
        let moved = load(&path, BasePolicy::ForceRelocation, false);
        let (m, r) = (&pref.modules[0], &moved.modules[0]);
        assert_eq!(m.base, m.preferred_base, "{arch}");
        assert_eq!(m.relocations_applied, 0);
        assert!(r.rebased());
        assert_eq!(r.relocations_applied, r.image.relocations.len());
        assert_relocations_consistent(&pref, &moved);

        // Some relocated pointer (the `lines` table) still leads to the message.
        let ptr = moved.format.pointer_size();
        let hits = r
            .image
            .relocations
            .iter()
            .filter_map(|rel| moved.memory.read_ptr_raw(r.base + rel.rva as u64, ptr).ok())
            .filter(|&target| {
                moved.memory.read_cstr(target, 64).ok().as_deref()
                    == Some(b"hello from anyos fixture\n")
            })
            .count();
        assert!(
            hits >= 1,
            "{arch}: no relocated pointer reaches the message"
        );

        assert_eq!(
            r.entry_point,
            Some(r.base + r.image.entry_point_rva.unwrap() as u64)
        );
        if r.image.format == PeFormat::Pe32 {
            assert!(
                r.base + r.size <= 0x8000_0000,
                "PE32 images stay in a 2 GiB address space"
            );
        }
    }
}

#[test]
fn explicit_base_is_honoured() {
    let path = fixtures().join("bin/x64/hello_console.exe");
    let p = load(&path, BasePolicy::At(0x7ff6_0000_0000), false);
    assert_eq!(p.modules[0].base, 0x7ff6_0000_0000);
}

#[test]
fn page_protections_follow_sections() {
    for arch in ARCHES {
        let p = load(
            &fixtures().join(format!("bin/{arch}/hello_console.exe")),
            BasePolicy::Preferred,
            false,
        );
        let m = &p.modules[0];
        let entry = m.entry_point.unwrap();
        p.memory.check_execute(entry).unwrap();
        assert_eq!(
            p.memory.protection_at(m.base).unwrap(),
            Protection::R,
            "headers are read-only"
        );

        let mut mem = p.memory;
        assert!(matches!(
            mem.write(entry, &[0x90]),
            Err(MemoryError::AccessViolation {
                access: Access::Write,
                ..
            })
        ));
        let data = m.sections.iter().find(|s| s.name == ".data").unwrap();
        assert_eq!(data.protection, Protection::RW);
        mem.write(data.address, &[1, 2, 3, 4]).unwrap();
        let rdata = m.sections.iter().find(|s| s.name == ".rdata").unwrap();
        assert!(
            mem.check_execute(rdata.address).is_err(),
            "{arch}: data is not executable"
        );
    }
}

#[test]
fn system_imports_bind_to_call_gate_stubs() {
    for arch in ARCHES {
        let p = load(
            &fixtures().join(format!("bin/{arch}/hello_console.exe")),
            BasePolicy::Preferred,
            false,
        );
        let m = &p.modules[0];
        assert_eq!(m.imports.len(), 3);
        let ptr = p.format.pointer_size();
        for imp in &m.imports {
            let slot = p.memory.read_ptr_raw(imp.iat_address, ptr).unwrap();
            assert_eq!(slot, imp.target);
            let entry = p.call_gate.lookup(slot).expect("stub");
            assert_eq!(entry.dll, "kernel32.dll");
            assert_eq!(entry.symbol, imp.symbol);
            assert!(matches!(imp.binding, Binding::CallGate { .. }));
            // Stubs are execute-only traps.
            assert_eq!(p.memory.protection_at(slot).unwrap(), Protection::X);
            let mut b = [0u8; 4];
            p.memory.read_raw(slot, &mut b).unwrap();
            match arch {
                "arm64" => assert_eq!(u32::from_le_bytes(b), 0xD43E_0000, "BRK #0xF000"),
                _ => assert_eq!(b, [0xCC; 4], "INT3"),
            }
        }
        // Distinct APIs get distinct stubs.
        let mut targets: Vec<u64> = m.imports.iter().map(|i| i.target).collect();
        targets.dedup();
        assert_eq!(targets.len(), 3);
    }
}

#[test]
fn application_dlls_are_loaded_and_bound_by_name_and_ordinal() {
    for arch in ARCHES {
        let p = load(
            &fixtures().join(format!("bin/{arch}/uses_dll.exe")),
            BasePolicy::Preferred,
            true,
        );
        assert_eq!(p.modules.len(), 2, "{arch}");
        let dll = p
            .module("EXPORTS_DLL")
            .expect("case-insensitive lookup without .dll");
        let rva_of = |sym: &ImportName| match dll.find_export(sym) {
            Some(ExportTarget::Rva { rva }) => *rva as u64,
            other => panic!("{other:?}"),
        };
        let exe = &p.modules[0];
        let add = exe
            .imports
            .iter()
            .find(|i| i.symbol.to_string() == "anyos_add")
            .unwrap();
        assert_eq!(add.target, dll.base + rva_of(&add.symbol));
        assert_eq!(
            add.binding,
            Binding::Module {
                module: "exports_dll.dll".into()
            }
        );

        let hidden = exe
            .imports
            .iter()
            .find(|i| i.symbol == ImportName::Ordinal { ordinal: 7 })
            .unwrap();
        assert_eq!(hidden.target, dll.base + rva_of(&hidden.symbol));

        let exit = exe
            .imports
            .iter()
            .find(|i| i.symbol.to_string() == "ExitProcess")
            .unwrap();
        assert!(matches!(exit.binding, Binding::CallGate { .. }));

        // Without dependency loading the same imports fall back to stubs.
        let alone = load(
            &fixtures().join(format!("bin/{arch}/uses_dll.exe")),
            BasePolicy::Preferred,
            false,
        );
        assert_eq!(alone.modules.len(), 1);
        assert!(alone.modules[0]
            .imports
            .iter()
            .all(|i| matches!(i.binding, Binding::CallGate { .. })));
    }
}

#[test]
fn dll_entry_and_delay_imports_are_recorded() {
    for arch in ARCHES {
        let p = load(
            &fixtures().join(format!("bin/{arch}/hello_gui.exe")),
            BasePolicy::Preferred,
            false,
        );
        let m = &p.modules[0];
        assert_eq!(m.delay_load_dlls, ["user32.dll"]);
        assert!(
            m.imports.iter().all(|i| i.dll == "kernel32.dll"),
            "delay imports are not bound eagerly"
        );
    }
}

#[test]
fn tls_slot_and_callbacks_are_prepared() {
    for arch in ARCHES {
        let path = fixtures().join(format!("bin/{arch}/hello_console.exe"));
        for policy in [BasePolicy::Preferred, BasePolicy::ForceRelocation] {
            let p = load(&path, policy, false);
            let m = &p.modules[0];
            let tls = m.tls.as_ref().expect("TLS");
            assert_eq!(tls.index, 0);
            assert_eq!(tls.callbacks.len(), 1);
            let text = m.sections.iter().find(|s| s.name == ".text").unwrap();
            assert!(
                tls.callbacks[0] >= text.address && tls.callbacks[0] < text.address + text.size,
                "{arch} {policy:?}"
            );
            let mut template = [0u8; 8];
            p.memory
                .read_raw(tls.template_start, &mut template)
                .unwrap();
            assert_eq!(template, [1, 2, 3, 4, 5, 6, 7, 8]);
        }
    }
}

#[test]
fn refuses_what_it_cannot_load_safely() {
    let path = fixtures().join("bin/x64/hello_console.exe");
    let mut bytes = std::fs::read(&path).unwrap();

    // Mark relocations stripped: the image can then only live at its preferred base.
    let pe = u32::from_le_bytes(bytes[0x3c..0x40].try_into().unwrap()) as usize;
    bytes[pe + 22] |= 0x01;
    let image = anyos_pe::parse(&bytes).unwrap();
    let mut p = Process::for_image(&image);
    let err = p
        .map_image(
            "fixed.exe",
            None,
            &bytes,
            image.clone(),
            BasePolicy::ForceRelocation,
        )
        .unwrap_err();
    assert!(matches!(err, LoadError::NotRelocatable(_)), "{err}");
    p.map_image(
        "fixed.exe",
        None,
        &bytes,
        image.clone(),
        BasePolicy::Preferred,
    )
    .unwrap();
    assert!(matches!(
        p.map_image("fixed.exe", None, &bytes, image, BasePolicy::Preferred),
        Err(LoadError::AlreadyLoaded(_))
    ));

    // A DLL for another CPU cannot join the process.
    let arm = std::fs::read(fixtures().join("bin/arm64/exports_dll.dll")).unwrap();
    let arm_img = anyos_pe::parse(&arm).unwrap();
    assert!(matches!(
        p.map_image(
            "exports_dll.dll",
            None,
            &arm,
            arm_img,
            BasePolicy::Preferred
        ),
        Err(LoadError::ArchitectureMismatch { .. })
    ));

    // Damaged directories are refused.
    let mut broken = std::fs::read(&path).unwrap();
    let import_dir = pe + 24 + 112 + 8;
    broken[import_dir..import_dir + 4].copy_from_slice(&0x00ff_0000u32.to_le_bytes());
    let img = anyos_pe::parse(&broken).unwrap();
    let mut p = Process::for_image(&img);
    assert!(matches!(
        p.map_image("broken.exe", None, &broken, img, BasePolicy::Preferred),
        Err(LoadError::BrokenDirectory(d)) if d == "import"
    ));
    assert_eq!(p.memory.mapped_bytes(), 0, "nothing left mapped");
}

#[test]
fn mutated_images_never_panic_the_loader() {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut loaded = 0;
    for arch in ARCHES {
        for name in ["hello_console.exe", "exports_dll.dll", "uses_dll.exe"] {
            let original = std::fs::read(fixtures().join(format!("bin/{arch}/{name}"))).unwrap();
            for _ in 0..400 {
                let mut b = original.clone();
                for _ in 0..(1 + next() % 6) {
                    let i = (next() % b.len() as u64) as usize;
                    b[i] = next() as u8;
                }
                let Ok(img) = anyos_pe::parse(&b) else {
                    continue;
                };
                let mut p = Process::for_image(&img);
                let policy = if next() % 2 == 0 {
                    BasePolicy::Preferred
                } else {
                    BasePolicy::ForceRelocation
                };
                if p.map_image(name, None, &b, img, policy).is_ok() && p.bind_all().is_ok() {
                    loaded += 1;
                }
            }
        }
    }
    assert!(loaded > 0);
}

/// Loads every .exe/.dll under `$ANYOS_CORPUS` (real-world binaries are not
/// committed). Run with: `ANYOS_CORPUS=/path cargo test -p anyos-loader -- --ignored`.
#[test]
#[ignore]
fn real_world_corpus() {
    let Some(root) = std::env::var_os("ANYOS_CORPUS") else {
        eprintln!("ANYOS_CORPUS not set; skipping");
        return;
    };
    let mut files = Vec::new();
    let mut stack = vec![PathBuf::from(root)];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("exe") || x.eq_ignore_ascii_case("dll"))
            {
                files.push(p);
            }
        }
    }
    files.sort();
    assert!(!files.is_empty());
    for f in &files {
        let a = load(f, BasePolicy::Preferred, false);
        let b = load(f, BasePolicy::ForceRelocation, false);
        assert_relocations_consistent(&a, &b);
        eprintln!(
            "ok  {:<70} {} {:>6} relocs {:>4} imports",
            f.display(),
            a.modules[0].image.machine.display_name(),
            b.modules[0].relocations_applied,
            a.modules[0].imports.len()
        );
    }
    eprintln!(
        "{} images loaded at preferred and forced bases",
        files.len()
    );
}
