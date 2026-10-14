//! Turns raw bytes into a [`PeImage`].
//!
//! goblin parses the DOS/NT headers and the section table; the data directories are
//! read here through [`ImageView`] so every offset and count is checked against our
//! own limits and the structures stay under our control.

use goblin::pe::header::Header;

use crate::consts::*;
use crate::model::*;
use crate::view::{add, ImageView};
use crate::{Limits, PeError};

pub(crate) fn parse(bytes: &[u8], limits: &Limits) -> Result<PeImage, PeError> {
    if bytes.len() as u64 > limits.max_file_size {
        return Err(PeError::LimitExceeded(format!(
            "file is {} bytes; limit is {}",
            bytes.len(),
            limits.max_file_size
        )));
    }
    if bytes.len() < 2 || &bytes[..2] != b"MZ" {
        return Err(PeError::NotPe("missing MZ signature".into()));
    }
    let header = Header::parse(bytes).map_err(goblin_err)?;
    if header.signature != 0x0000_4550 {
        return Err(PeError::NotPe("missing PE\\0\\0 signature".into()));
    }
    let coff = header.coff_header;
    let opt = header.optional_header.ok_or_else(|| {
        PeError::Unsupported("no optional header (COFF object, not an image)".into())
    })?;

    let format = match opt.standard_fields.magic {
        0x10b => PeFormat::Pe32,
        0x20b => PeFormat::Pe32Plus,
        m => {
            return Err(PeError::Unsupported(format!(
                "optional header magic 0x{m:x} (ROM image?)"
            )))
        }
    };
    if coff.number_of_sections as usize > limits.max_sections {
        return Err(PeError::LimitExceeded(format!(
            "{} sections; limit is {}",
            coff.number_of_sections, limits.max_sections
        )));
    }

    let wf = &opt.windows_fields;
    let mut warnings = Vec::new();

    let mut offset =
        header.dos_header.pe_pointer as usize + 4 + 20 + coff.size_of_optional_header as usize;
    let raw_sections = coff.sections(bytes, &mut offset).map_err(goblin_err)?;
    let sections: Vec<Section> = raw_sections
        .iter()
        .map(|s| Section {
            name: s.name().unwrap_or("?").trim_end_matches('\0').to_string(),
            virtual_address: s.virtual_address,
            virtual_size: s.virtual_size,
            raw_offset: s.pointer_to_raw_data,
            raw_size: s.size_of_raw_data,
            characteristics: s.characteristics,
            access: SectionAccess::from_characteristics(s.characteristics),
        })
        .collect();

    validate_layout(
        wf.size_of_image,
        wf.section_alignment,
        wf.file_alignment,
        wf.size_of_headers,
        &sections,
        limits,
        &mut warnings,
    )?;

    let mut data_directories =
        vec![DataDirectory { rva: 0, size: 0 }; IMAGE_NUMBEROF_DIRECTORY_ENTRIES];
    for (i, dd) in opt
        .data_directories
        .data_directories
        .iter()
        .enumerate()
        .take(IMAGE_NUMBEROF_DIRECTORY_ENTRIES)
    {
        if let Some((_, d)) = dd {
            data_directories[i] = DataDirectory {
                rva: d.virtual_address,
                size: d.size,
            };
        }
    }
    let dir = |i: usize| data_directories[i];
    let present = |i: usize| data_directories[i].rva != 0 && data_directories[i].size != 0;

    let entry = opt.standard_fields.address_of_entry_point;
    let entry_point_rva = (entry != 0).then_some(entry);
    if let Some(e) = entry_point_rva {
        if e >= wf.size_of_image {
            warnings.push(format!("entry point RVA 0x{e:x} lies outside the image"));
        }
    }

    let machine = Machine::from_raw(coff.machine);
    let view = ImageView::new(bytes, &sections, wf.size_of_headers, wf.file_alignment);
    let ctx = Ctx {
        view: &view,
        limits,
        ptr_size: format.pointer_size(),
        image_base: wf.image_base,
        machine,
    };

    let mut broken_directories = Vec::new();
    let mut note = |name: &str, e: PeError, broken: &mut Vec<String>| {
        broken.push(name.to_string());
        warnings.push(format!("{name} directory: {e}"));
    };

    let mut imports = Vec::new();
    if present(IMAGE_DIRECTORY_ENTRY_IMPORT) {
        match parse_imports(&ctx, dir(IMAGE_DIRECTORY_ENTRY_IMPORT)) {
            Ok(m) => imports.extend(m),
            Err(e) => note("import", e, &mut broken_directories),
        }
    }
    if present(IMAGE_DIRECTORY_ENTRY_DELAY_IMPORT) {
        match parse_delay_imports(&ctx, dir(IMAGE_DIRECTORY_ENTRY_DELAY_IMPORT)) {
            Ok(m) => imports.extend(m),
            Err(e) => note("delay-import", e, &mut broken_directories),
        }
    }
    let exports = if present(IMAGE_DIRECTORY_ENTRY_EXPORT) {
        match parse_exports(&ctx, dir(IMAGE_DIRECTORY_ENTRY_EXPORT)) {
            Ok(t) => Some(t),
            Err(e) => {
                note("export", e, &mut broken_directories);
                None
            }
        }
    } else {
        None
    };
    let relocations = if present(IMAGE_DIRECTORY_ENTRY_BASERELOC) {
        match parse_relocations(&ctx, dir(IMAGE_DIRECTORY_ENTRY_BASERELOC)) {
            Ok(r) => r,
            Err(e) => {
                note("base-relocation", e, &mut broken_directories);
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    let tls = if present(IMAGE_DIRECTORY_ENTRY_TLS) {
        match parse_tls(&ctx, dir(IMAGE_DIRECTORY_ENTRY_TLS)) {
            Ok(t) => Some(t),
            Err(e) => {
                note("tls", e, &mut broken_directories);
                None
            }
        }
    } else {
        None
    };
    let clr = if present(IMAGE_DIRECTORY_ENTRY_COM_DESCRIPTOR) {
        match parse_clr(&ctx, dir(IMAGE_DIRECTORY_ENTRY_COM_DESCRIPTOR)) {
            Ok(c) => Some(c),
            Err(e) => {
                note("clr", e, &mut broken_directories);
                None
            }
        }
    } else {
        None
    };

    // Bytes after the last piece of file-backed data. The certificate table is
    // addressed by file offset and normally lives here too.
    let mut data_end = wf.size_of_headers as u64;
    for s in &sections {
        if s.raw_size > 0 {
            data_end = data_end.max(view.raw_pointer(s) as u64 + s.raw_size as u64);
        }
    }
    let overlay_size = (bytes.len() as u64).saturating_sub(data_end);

    Ok(PeImage {
        format,
        machine,
        machine_raw: coff.machine,
        is_dll: coff.characteristics & IMAGE_FILE_DLL != 0,
        subsystem: Subsystem::from_raw(wf.subsystem),
        file_size: bytes.len() as u64,
        timestamp: coff.time_date_stamp,
        characteristics: coff.characteristics,
        dll_characteristics: wf.dll_characteristics,
        linker_version: (
            opt.standard_fields.major_linker_version,
            opt.standard_fields.minor_linker_version,
        ),
        os_version: (
            wf.major_operating_system_version,
            wf.minor_operating_system_version,
        ),
        subsystem_version: (wf.major_subsystem_version, wf.minor_subsystem_version),
        image_base: wf.image_base,
        size_of_image: wf.size_of_image,
        size_of_headers: wf.size_of_headers,
        section_alignment: wf.section_alignment,
        file_alignment: wf.file_alignment,
        entry_point_rva,
        stack_reserve: wf.size_of_stack_reserve,
        stack_commit: wf.size_of_stack_commit,
        heap_reserve: wf.size_of_heap_reserve,
        heap_commit: wf.size_of_heap_commit,
        sections,
        has_resources: present(IMAGE_DIRECTORY_ENTRY_RESOURCE),
        has_exception_table: present(IMAGE_DIRECTORY_ENTRY_EXCEPTION),
        has_load_config: present(IMAGE_DIRECTORY_ENTRY_LOAD_CONFIG),
        has_signature: present(IMAGE_DIRECTORY_ENTRY_SECURITY),
        has_debug_info: present(IMAGE_DIRECTORY_ENTRY_DEBUG),
        data_directories,
        imports,
        exports,
        relocations,
        tls,
        clr,
        overlay_size,
        broken_directories,
        warnings,
    })
}

fn goblin_err(e: goblin::error::Error) -> PeError {
    match e {
        goblin::error::Error::BadMagic(m) => PeError::NotPe(format!("bad magic 0x{m:x}")),
        goblin::error::Error::Scroll(_) | goblin::error::Error::BufferTooShort(..) => {
            PeError::Truncated(e.to_string())
        }
        other => PeError::Malformed(other.to_string()),
    }
}

fn validate_layout(
    size_of_image: u32,
    section_alignment: u32,
    file_alignment: u32,
    size_of_headers: u32,
    sections: &[Section],
    limits: &Limits,
    warnings: &mut Vec<String>,
) -> Result<(), PeError> {
    if size_of_image == 0 {
        return Err(PeError::Malformed("SizeOfImage is zero".into()));
    }
    if size_of_image as u64 > limits.max_image_size {
        return Err(PeError::LimitExceeded(format!(
            "SizeOfImage 0x{size_of_image:x} exceeds limit 0x{:x}",
            limits.max_image_size
        )));
    }
    if !section_alignment.is_power_of_two() || !file_alignment.is_power_of_two() {
        return Err(PeError::Malformed(format!(
            "alignments must be powers of two (section 0x{section_alignment:x}, file 0x{file_alignment:x})"
        )));
    }
    if size_of_headers > size_of_image {
        return Err(PeError::Malformed(
            "SizeOfHeaders exceeds SizeOfImage".into(),
        ));
    }
    let mut prev_end = 0u64;
    for s in sections {
        let end = s.virtual_address as u64 + s.mapped_size() as u64;
        if end > size_of_image as u64 {
            return Err(PeError::Malformed(format!(
                "section {:?} ends at 0x{end:x}, past SizeOfImage 0x{size_of_image:x}",
                s.name
            )));
        }
        if (s.virtual_address as u64) < prev_end {
            return Err(PeError::Malformed(format!(
                "section {:?} overlaps the previous section",
                s.name
            )));
        }
        if s.virtual_address < size_of_headers {
            warnings.push(format!("section {:?} starts inside the headers", s.name));
        }
        prev_end = end;
    }
    Ok(())
}

struct Ctx<'a, 'b> {
    view: &'b ImageView<'a>,
    limits: &'b Limits,
    ptr_size: usize,
    image_base: u64,
    machine: Machine,
}

impl Ctx<'_, '_> {
    fn ordinal_flag(&self) -> u64 {
        if self.ptr_size == 8 {
            1 << 63
        } else {
            1 << 31
        }
    }

    fn va_to_rva(&self, va: u64) -> Result<u32, PeError> {
        va.checked_sub(self.image_base)
            .and_then(|r| u32::try_from(r).ok())
            .ok_or_else(|| PeError::Malformed(format!("VA 0x{va:x} is outside the image")))
    }

    /// Reads an import lookup table (normal or delay-load) starting at `ilt_rva`.
    /// IAT slots are at the same index from `iat_rva`.
    fn thunks(&self, ilt_rva: u32, iat_rva: u32) -> Result<Vec<ImportedSymbol>, PeError> {
        let mut out = Vec::new();
        for i in 0u64.. {
            if out.len() >= self.limits.max_imports_per_module {
                return Err(PeError::LimitExceeded(format!(
                    "more than {} imports from one module",
                    self.limits.max_imports_per_module
                )));
            }
            let thunk = self
                .view
                .ptr(add(ilt_rva, i * self.ptr_size as u64)?, self.ptr_size)?;
            if thunk == 0 {
                break;
            }
            let name = if thunk & self.ordinal_flag() != 0 {
                ImportName::Ordinal {
                    ordinal: (thunk & 0xffff) as u16,
                }
            } else {
                let hn = (thunk & 0x7fff_ffff) as u32;
                ImportName::Name {
                    hint: self.view.u16(hn)?,
                    name: self.view.cstr(add(hn, 2)?, MAX_NAME)?,
                }
            };
            out.push(ImportedSymbol {
                name,
                iat_rva: add(iat_rva, i * self.ptr_size as u64)?,
            });
        }
        Ok(out)
    }
}

const MAX_NAME: usize = 4096;

fn parse_imports(ctx: &Ctx, dir: DataDirectory) -> Result<Vec<ImportedModule>, PeError> {
    let mut modules = Vec::new();
    for i in 0u64.. {
        let d = add(dir.rva, i * 20)?;
        let ilt = ctx.view.u32(d)?;
        let name_rva = ctx.view.u32(add(d, 12)?)?;
        let iat = ctx.view.u32(add(d, 16)?)?;
        if ilt == 0 && name_rva == 0 && iat == 0 {
            break;
        }
        if modules.len() >= ctx.limits.max_import_modules {
            return Err(PeError::LimitExceeded(format!(
                "more than {} imported modules",
                ctx.limits.max_import_modules
            )));
        }
        if iat == 0 {
            return Err(PeError::Malformed(format!(
                "import descriptor {i} has no import address table"
            )));
        }
        let dll = ctx.view.cstr(name_rva, MAX_NAME)?;
        // Old Borland linkers leave OriginalFirstThunk empty; the IAT then doubles as the lookup table.
        let lookup = if ilt != 0 { ilt } else { iat };
        modules.push(ImportedModule {
            dll,
            delay_load: false,
            symbols: ctx.thunks(lookup, iat)?,
        });
    }
    Ok(modules)
}

fn parse_delay_imports(ctx: &Ctx, dir: DataDirectory) -> Result<Vec<ImportedModule>, PeError> {
    let mut modules = Vec::new();
    for i in 0u64.. {
        let d = add(dir.rva, i * 32)?;
        let attributes = ctx.view.u32(d)?;
        let name = ctx.view.u32(add(d, 4)?)?;
        let iat = ctx.view.u32(add(d, 12)?)?;
        let int = ctx.view.u32(add(d, 16)?)?;
        if name == 0 && iat == 0 && int == 0 {
            break;
        }
        if modules.len() >= ctx.limits.max_import_modules {
            return Err(PeError::LimitExceeded(format!(
                "more than {} delay-load modules",
                ctx.limits.max_import_modules
            )));
        }
        // Attribute bit 0 set = fields are RVAs; clear = legacy VC6 images storing VAs.
        let fix = |v: u32| -> Result<u32, PeError> {
            if attributes & 1 != 0 {
                Ok(v)
            } else {
                ctx.va_to_rva(v as u64)
            }
        };
        let dll = ctx.view.cstr(fix(name)?, MAX_NAME)?;
        modules.push(ImportedModule {
            dll,
            delay_load: true,
            symbols: ctx.thunks(fix(int)?, fix(iat)?)?,
        });
    }
    Ok(modules)
}

fn parse_exports(ctx: &Ctx, dir: DataDirectory) -> Result<ExportTable, PeError> {
    let v = ctx.view;
    let name_rva = v.u32(add(dir.rva, 12)?)?;
    let ordinal_base = v.u32(add(dir.rva, 16)?)?;
    let n_functions = v.u32(add(dir.rva, 20)?)? as usize;
    let n_names = v.u32(add(dir.rva, 24)?)? as usize;
    let functions = v.u32(add(dir.rva, 28)?)?;
    let names = v.u32(add(dir.rva, 32)?)?;
    let name_ordinals = v.u32(add(dir.rva, 36)?)?;
    if n_functions > ctx.limits.max_exports || n_names > ctx.limits.max_exports {
        return Err(PeError::LimitExceeded(format!(
            "{n_functions} export slots / {n_names} names; limit is {}",
            ctx.limits.max_exports
        )));
    }

    let mut slot_names: Vec<Option<String>> = vec![None; n_functions];
    for i in 0..n_names as u64 {
        let idx = v.u16(add(name_ordinals, i * 2)?)? as usize;
        let name = v.cstr(v.u32(add(names, i * 4)?)?, MAX_NAME)?;
        match slot_names.get_mut(idx) {
            Some(slot) if slot.is_none() => *slot = Some(name),
            Some(_) => {} // aliases: keep the first name
            None => {
                return Err(PeError::Malformed(format!(
                    "export name {name:?} points at slot {idx} of {n_functions}"
                )))
            }
        }
    }

    let dir_end = dir.rva as u64 + dir.size as u64;
    let mut exports = Vec::new();
    for (i, name) in slot_names.into_iter().enumerate() {
        let rva = v.u32(add(functions, i as u64 * 4)?)?;
        if rva == 0 {
            continue; // unused ordinal
        }
        let target = if (rva as u64) >= dir.rva as u64 && (rva as u64) < dir_end {
            ExportTarget::Forwarder {
                target: v.cstr(rva, MAX_NAME)?,
            }
        } else {
            ExportTarget::Rva { rva }
        };
        exports.push(Export {
            ordinal: ordinal_base.wrapping_add(i as u32),
            name,
            target,
        });
    }
    let dll_name = if name_rva != 0 {
        v.cstr(name_rva, MAX_NAME).ok()
    } else {
        None
    };
    Ok(ExportTable {
        dll_name,
        ordinal_base,
        exports,
    })
}

fn parse_relocations(ctx: &Ctx, dir: DataDirectory) -> Result<Vec<BaseRelocation>, PeError> {
    let v = ctx.view;
    let mut out = Vec::new();
    let mut pos: u64 = 0;
    while pos + 8 <= dir.size as u64 {
        let block = add(dir.rva, pos)?;
        let page = v.u32(block)?;
        let block_size = v.u32(add(block, 4)?)? as u64;
        if block_size == 0 {
            break; // trailing padding emitted by some linkers
        }
        if block_size < 8 || !block_size.is_multiple_of(2) || pos + block_size > dir.size as u64 {
            return Err(PeError::Malformed(format!(
                "relocation block at +0x{pos:x} has bad size {block_size}"
            )));
        }
        let count = (block_size - 8) / 2;
        let entries = v.slice(add(block, 8)?, (count * 2) as usize)?;
        let mut j = 0usize;
        while j < count as usize {
            let e = u16::from_le_bytes([entries[j * 2], entries[j * 2 + 1]]);
            j += 1;
            let ty = (e >> 12) as u8;
            let rva = add(page, (e & 0x0fff) as u64)?;
            let kind = match ty {
                IMAGE_REL_BASED_ABSOLUTE => continue,
                IMAGE_REL_BASED_HIGH => RelocationKind::High,
                IMAGE_REL_BASED_LOW => RelocationKind::Low,
                IMAGE_REL_BASED_HIGHLOW => RelocationKind::HighLow,
                IMAGE_REL_BASED_HIGHADJ => {
                    let b = entries.get(j * 2..j * 2 + 2).ok_or_else(|| {
                        PeError::Malformed("HIGHADJ relocation without its parameter".into())
                    })?;
                    j += 1;
                    RelocationKind::HighAdj {
                        adjust: u16::from_le_bytes([b[0], b[1]]),
                    }
                }
                IMAGE_REL_BASED_DIR64 => RelocationKind::Dir64,
                IMAGE_REL_BASED_ARM_MOV32 if ctx.machine == Machine::ArmNt => {
                    RelocationKind::ArmMov32
                }
                IMAGE_REL_BASED_THUMB_MOV32 if ctx.machine == Machine::ArmNt => {
                    RelocationKind::ThumbMov32
                }
                other => RelocationKind::Unsupported(other),
            };
            if out.len() >= ctx.limits.max_relocations {
                return Err(PeError::LimitExceeded(format!(
                    "more than {} relocations",
                    ctx.limits.max_relocations
                )));
            }
            out.push(BaseRelocation { rva, kind });
        }
        pos += block_size;
    }
    Ok(out)
}

fn parse_tls(ctx: &Ctx, dir: DataDirectory) -> Result<TlsDirectory, PeError> {
    let v = ctx.view;
    let p = ctx.ptr_size as u64;
    let start = v.ptr(dir.rva, ctx.ptr_size)?;
    let end = v.ptr(add(dir.rva, p)?, ctx.ptr_size)?;
    let index = v.ptr(add(dir.rva, 2 * p)?, ctx.ptr_size)?;
    let callbacks_va = v.ptr(add(dir.rva, 3 * p)?, ctx.ptr_size)?;
    let zero_fill_size = v.u32(add(dir.rva, 4 * p)?)?;
    let characteristics = v.u32(add(dir.rva, 4 * p + 4)?)?;
    if end < start {
        return Err(PeError::Malformed(
            "TLS raw data ends before it starts".into(),
        ));
    }
    let mut callbacks = Vec::new();
    if callbacks_va != 0 {
        let base = ctx.va_to_rva(callbacks_va)?;
        loop {
            let cb = v.ptr(add(base, callbacks.len() as u64 * p)?, ctx.ptr_size)?;
            if cb == 0 {
                break;
            }
            if callbacks.len() >= ctx.limits.max_tls_callbacks {
                return Err(PeError::LimitExceeded(format!(
                    "more than {} TLS callbacks",
                    ctx.limits.max_tls_callbacks
                )));
            }
            callbacks.push(cb);
        }
    }
    Ok(TlsDirectory {
        raw_data_start_va: start,
        raw_data_end_va: end,
        index_va: index,
        callbacks_va,
        zero_fill_size,
        characteristics,
        callbacks,
    })
}

fn parse_clr(ctx: &Ctx, dir: DataDirectory) -> Result<ClrInfo, PeError> {
    let v = ctx.view;
    let cb = v.u32(dir.rva)?;
    if cb < 72 {
        return Err(PeError::Malformed(format!(
            "CLR header size {cb} is smaller than 72"
        )));
    }
    let runtime_major = v.u16(add(dir.rva, 4)?)?;
    let runtime_minor = v.u16(add(dir.rva, 6)?)?;
    let md_rva = v.u32(add(dir.rva, 8)?)?;
    let flags = v.u32(add(dir.rva, 16)?)?;
    let entry_point_token = v.u32(add(dir.rva, 20)?)?;
    let native_header_rva = v.u32(add(dir.rva, 64)?)?;

    // Metadata root: 'BSJB', versions, reserved, length, version string.
    let metadata_version = (|| -> Result<Option<String>, PeError> {
        if md_rva == 0 || v.u32(md_rva)? != 0x424A_5342 {
            return Ok(None);
        }
        let len = v.u32(add(md_rva, 12)?)? as usize;
        let raw = v.slice(add(md_rva, 16)?, len.min(255))?;
        let s = raw.split(|&b| b == 0).next().unwrap_or_default();
        Ok(Some(String::from_utf8_lossy(s).into_owned()))
    })()?;

    Ok(ClrInfo {
        runtime_major,
        runtime_minor,
        flags,
        metadata_version,
        il_only: flags & COMIMAGE_FLAGS_ILONLY != 0,
        requires_32bit: flags & COMIMAGE_FLAGS_32BITREQUIRED != 0,
        prefers_32bit: flags & COMIMAGE_FLAGS_32BITPREFERRED != 0,
        strong_name_signed: flags & COMIMAGE_FLAGS_STRONGNAMESIGNED != 0,
        native_entry_point: flags & COMIMAGE_FLAGS_NATIVE_ENTRYPOINT != 0,
        entry_point_token,
        has_native_code_header: native_header_rva != 0,
    })
}
