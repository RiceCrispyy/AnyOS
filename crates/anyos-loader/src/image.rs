//! Mapping, relocation, TLS setup and page protection for one image.

use std::collections::HashMap;
use std::path::PathBuf;

use anyos_memory::{round_up, Protection, PAGE_SIZE};
use anyos_pe::{PeError, PeImage, RelocationKind};

use crate::{BasePolicy, LoadError, LoadedModule, LoadedTls, MappedSection, Process};

pub(crate) fn map(
    process: &mut Process,
    name: &str,
    path: Option<PathBuf>,
    bytes: &[u8],
    image: PeImage,
    policy: BasePolicy,
) -> Result<LoadedModule, LoadError> {
    if let Some(d) = image.broken_directories.first() {
        return Err(LoadError::BrokenDirectory(d.clone()));
    }
    let mem = &mut process.memory;
    let size = round_up(image.size_of_image as u64, PAGE_SIZE);
    let preferred = image.image_base;
    let relocatable = !image.relocations_stripped();
    let tag = format!("image {name}");

    let base = match policy {
        BasePolicy::Preferred
            if preferred.is_multiple_of(PAGE_SIZE) && mem.is_free(preferred, size) =>
        {
            mem.map(preferred, size, Protection::RW, &tag)?;
            preferred
        }
        BasePolicy::Preferred | BasePolicy::ForceRelocation => {
            if !relocatable {
                return Err(LoadError::NotRelocatable(preferred));
            }
            let hint = (policy == BasePolicy::Preferred).then_some(preferred);
            let mut b = mem
                .map_anywhere(size, Protection::RW, &tag, hint)
                .or_else(|_| mem.map_anywhere(size, Protection::RW, &tag, None))?;
            if b == preferred && policy == BasePolicy::ForceRelocation {
                // Extremely unlikely, but honour the request.
                let other = mem.map_anywhere(size, Protection::RW, &tag, None)?;
                mem.unmap(b)?;
                b = other;
            }
            b
        }
        BasePolicy::At(addr) => {
            if addr != preferred && !relocatable {
                return Err(LoadError::NotRelocatable(preferred));
            }
            mem.map(addr, size, Protection::RW, &tag)?;
            addr
        }
    };

    // Any failure past this point must not leave a half-built image mapped.
    let result = populate(process, base, bytes, &image);
    if let Err(e) = result {
        let _ = process.memory.unmap(base);
        return Err(e);
    }
    let relocations_applied = result.expect("checked");

    let low_alignment = (image.section_alignment as u64) < PAGE_SIZE;
    let sections = image
        .sections
        .iter()
        .filter(|s| s.mapped_size() > 0)
        .map(|s| MappedSection {
            name: s.name.clone(),
            address: base + s.virtual_address as u64,
            size: s.mapped_size() as u64,
            protection: if low_alignment {
                Protection::RWX
            } else {
                section_protection(s.access)
            },
        })
        .collect();

    let exports_by_name = image
        .exports
        .iter()
        .flat_map(|t| t.exports.iter().enumerate())
        .filter_map(|(i, e)| e.name.clone().map(|n| (n, i)))
        .collect::<HashMap<_, _>>();

    Ok(LoadedModule {
        name: name.to_string(),
        path,
        base,
        size,
        preferred_base: preferred,
        relocations_applied,
        entry_point: image.entry_point_rva.map(|rva| base + rva as u64),
        sections,
        imports: Vec::new(),
        delay_load_dlls: image
            .imports
            .iter()
            .filter(|m| m.delay_load)
            .map(|m| m.dll.clone())
            .collect(),
        tls: None,
        stack_reserve: image.stack_reserve,
        stack_commit: image.stack_commit,
        image,
        exports_by_name,
        bound: false,
    })
}

/// Copy headers and section data, then relocate. Returns relocations applied.
fn populate(
    process: &mut Process,
    base: u64,
    bytes: &[u8],
    image: &PeImage,
) -> Result<usize, LoadError> {
    let mem = &mut process.memory;
    let headers = (image.size_of_headers as usize)
        .min(bytes.len())
        .min(image.size_of_image as usize);
    mem.write_raw(base, &bytes[..headers])?;

    for s in &image.sections {
        if s.raw_size == 0 || s.raw_offset == 0 {
            continue; // uninitialized data: already zero
        }
        let raw = if image.file_alignment >= 0x200 {
            s.raw_offset & !0x1ff
        } else {
            s.raw_offset
        } as usize;
        let len = (s.raw_size.min(s.mapped_size())) as usize;
        let data = raw
            .checked_add(len)
            .and_then(|end| bytes.get(raw..end))
            .ok_or_else(|| {
                PeError::Truncated(format!(
                    "section {:?} raw data runs past end of file",
                    s.name
                ))
            })?;
        mem.write_raw(base + s.virtual_address as u64, data)?;
    }

    let delta = base.wrapping_sub(image.image_base);
    if delta == 0 {
        return Ok(0);
    }
    for r in &image.relocations {
        let width = match r.kind {
            RelocationKind::Dir64 => 8,
            RelocationKind::HighLow => 4,
            RelocationKind::High | RelocationKind::Low | RelocationKind::HighAdj { .. } => 2,
            RelocationKind::ArmMov32 => {
                return Err(LoadError::UnsupportedRelocation {
                    kind: 5,
                    rva: r.rva,
                })
            }
            RelocationKind::ThumbMov32 => {
                return Err(LoadError::UnsupportedRelocation {
                    kind: 7,
                    rva: r.rva,
                })
            }
            RelocationKind::Unsupported(kind) => {
                return Err(LoadError::UnsupportedRelocation { kind, rva: r.rva })
            }
        };
        if r.rva as u64 + width > image.size_of_image as u64 {
            return Err(LoadError::RelocationOutOfImage(r.rva));
        }
        let at = base + r.rva as u64;
        match r.kind {
            RelocationKind::Dir64 => {
                let v = mem.read_u64_raw(at)?;
                mem.write_raw(at, &v.wrapping_add(delta).to_le_bytes())?;
            }
            RelocationKind::HighLow => {
                let v = mem.read_u32_raw(at)?;
                mem.write_raw(at, &v.wrapping_add(delta as u32).to_le_bytes())?;
            }
            RelocationKind::Low => {
                let v = mem.read_u16_raw(at)?;
                mem.write_raw(at, &v.wrapping_add(delta as u16).to_le_bytes())?;
            }
            RelocationKind::High => {
                // As the Windows loader: treat the word as the high half of a 32-bit value.
                let v = ((mem.read_u16_raw(at)? as u32) << 16).wrapping_add(delta as u32);
                mem.write_raw(at, &((v >> 16) as u16).to_le_bytes())?;
            }
            RelocationKind::HighAdj { adjust } => {
                let v = ((mem.read_u16_raw(at)? as u32) << 16)
                    .wrapping_add(adjust as i16 as i32 as u32)
                    .wrapping_add(delta as u32)
                    .wrapping_add(0x8000);
                mem.write_raw(at, &((v >> 16) as u16).to_le_bytes())?;
            }
            _ => unreachable!("rejected above"),
        }
    }
    Ok(image.relocations.len())
}

fn section_protection(a: anyos_pe::SectionAccess) -> Protection {
    // x86 and ARM64 page tables have no write-only pages: writable implies readable.
    Protection {
        read: a.read || a.write,
        write: a.write,
        execute: a.execute,
    }
}

/// Give the module a TLS slot and record its relocated callbacks.
pub(crate) fn assign_tls(process: &mut Process, i: usize) -> Result<(), LoadError> {
    let m = &process.modules[i];
    let Some(tls) = m.image.tls.clone() else {
        return Ok(());
    };
    let ptr = process.format.pointer_size();
    let (base, preferred, size) = (m.base, m.preferred_base, m.image.size_of_image as u64);
    let rebase = |va: u64| va.wrapping_sub(preferred).wrapping_add(base);
    let inside = |va: u64| va >= base && va < base + size;

    let index = process.next_tls_index;
    process.next_tls_index += 1;
    if tls.index_va != 0 {
        let at = rebase(tls.index_va);
        if !inside(at) {
            return Err(LoadError::BrokenDirectory("tls".into()));
        }
        process.memory.write_raw(at, &index.to_le_bytes())?;
    }
    // Read callbacks back from guest memory: the array has already been relocated.
    let mut callbacks = Vec::with_capacity(tls.callbacks.len());
    if tls.callbacks_va != 0 {
        let array = rebase(tls.callbacks_va);
        for n in 0..tls.callbacks.len() as u64 {
            callbacks.push(process.memory.read_ptr_raw(array + n * ptr as u64, ptr)?);
        }
    }
    process.modules[i].tls = Some(LoadedTls {
        index,
        template_start: rebase(tls.raw_data_start_va),
        template_end: rebase(tls.raw_data_end_va),
        zero_fill: tls.zero_fill_size,
        callbacks,
    });
    Ok(())
}

/// Apply final page protections: headers read-only, sections per characteristics.
pub(crate) fn protect(process: &mut Process, i: usize) -> Result<(), LoadError> {
    let m = &process.modules[i];
    let mem = &mut process.memory;
    if (m.image.section_alignment as u64) < PAGE_SIZE {
        mem.protect(m.base, m.size, Protection::RWX)?;
        return Ok(());
    }
    mem.protect(m.base, m.size, Protection::R)?;
    for s in &m.sections {
        mem.protect(s.address, s.size, s.protection)?;
    }
    Ok(())
}
