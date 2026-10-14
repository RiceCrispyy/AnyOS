//! NSIS (Nullsoft Scriptable Install System) installers, unpacked without
//! running them.
//!
//! An NSIS installer is a small stub `.exe` followed by a "first header", a
//! compressed header block and the data blocks. The header holds the install
//! script compiled to entries (opcode plus six parameters) and a string table.
//! AnyOS reads the script in order, without following its control flow, and
//! acts on the entries that place files and settings:
//!
//! * `CreateDirectory` / `SetOutPath`, `ExtractFile` (the `File` command),
//!   `StrCpy` into variables (so paths built from variables resolve),
//! * `WriteRegStr` / `WriteRegExpandStr` / `WriteRegDWORD`,
//! * `CreateShortcut`.
//!
//! `$INSTDIR` is the installer's own default folder (preferring a 64-bit
//! `$PROGRAMFILES64` choice from `.onInit`). Files for the installer's own use
//! (`$PLUGINSDIR`, `$TEMP`) are not installed. Plugin calls, `Exec`, and
//! anything else that needs code to run are skipped and noted.
//!
//! Supported: NSIS 2 and 3, ANSI and Unicode, zlib (deflate) and LZMA, solid
//! and non-solid. NSIS's own bzip2 variant is refused.

use std::collections::HashMap;
use std::io::Read;

use anyos_env::{KnownFolder, RegValue, Shortcut};

use crate::{InstallError, Installed, Product, Writer};

const FIRST_HEADER_LEN: usize = 28;
const ENTRY_LEN: usize = 28;
/// Upper bound on a decompressed header or data stream.
const MAX_STREAM: usize = 4 << 30;

const BLOCK_ENTRIES: usize = 2;
const BLOCK_STRINGS: usize = 3;
const BLOCK_LANGTABLES: usize = 4;

const EW_CREATEDIR: i32 = 11;
const EW_EXTRACTFILE: i32 = 20;
const EW_ASSIGNVAR: i32 = 25;
const EW_EXECUTE: i32 = 41;
const EW_REGISTERDLL: i32 = 44;
const EW_CREATESHORTCUT: i32 = 45;
const EW_WRITEREG: i32 = 51;
const EW_WRITEUNINSTALLER: i32 = 62;

const VAR_INSTDIR: u16 = 21;
const VAR_OUTDIR: u16 = 22;
const VAR_LANGUAGE: u16 = 24;
const VAR_TEMP: u16 = 25;
const VAR_PLUGINSDIR: u16 = 26;

fn malformed(message: impl Into<String>) -> InstallError {
    InstallError::Malformed {
        what: "NSIS installer",
        message: message.into(),
    }
}

fn u32_at(b: &[u8], off: usize) -> Option<u32> {
    b.get(off..off + 4)
        .map(|s| u32::from_le_bytes(s.try_into().unwrap()))
}

fn i32_at(b: &[u8], off: usize) -> Option<i32> {
    u32_at(b, off).map(|v| v as i32)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Method {
    Lzma,
    Deflate,
    Stored,
}

/// Decompress a whole NSIS stream. LZMA streams carry a 5-byte properties
/// header and no size; they run to the end of the input.
fn decompress(method: Method, data: &[u8], limit: usize) -> Result<Vec<u8>, InstallError> {
    let mut out = Vec::new();
    match method {
        Method::Stored => out.extend_from_slice(data),
        Method::Deflate => {
            let mut r = flate2::read::DeflateDecoder::new(data).take(limit as u64);
            read_tolerant(&mut r, &mut out)?;
        }
        Method::Lzma => {
            let (props, rest) = data.split_first().ok_or_else(|| malformed("empty LZMA"))?;
            let dict = u32_at(rest, 0).ok_or_else(|| malformed("short LZMA header"))?;
            let r =
                lzma_rust2::LzmaReader::new_with_props(&rest[4..], u64::MAX, *props, dict, None)
                    .map_err(|e| malformed(format!("LZMA: {e}")))?;
            read_tolerant(&mut r.take(limit as u64), &mut out)?;
        }
    }
    Ok(out)
}

/// Read to the end, accepting an error once some output exists: NSIS LZMA
/// streams have no end marker, so the decoder may complain at the very end.
fn read_tolerant(r: &mut dyn Read, out: &mut Vec<u8>) -> Result<(), InstallError> {
    let mut buf = vec![0u8; 1 << 16];
    loop {
        match r.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(_) if !out.is_empty() => return Ok(()),
            Err(e) => return Err(malformed(format!("decompression failed: {e}"))),
        }
    }
}

fn lzma_start(b: &[u8]) -> bool {
    // Properties byte 0x5D (lc=3, lp=0, pb=2) and a sane dictionary size.
    b.first() == Some(&0x5D) && u32_at(b, 1).is_some_and(|d| (1 << 12..=1 << 30).contains(&d))
}

/// Where file data comes from.
enum Data<'a> {
    /// Solid: one stream; the data area follows the header in it.
    Solid(Vec<u8>),
    /// Non-solid: each block is compressed on its own.
    Blocks { area: &'a [u8], method: Method },
}

impl Data<'_> {
    fn block(&self, offset: u32) -> Result<Vec<u8>, InstallError> {
        let bad = || malformed(format!("data block at {offset} is out of range"));
        let off = offset as usize;
        match self {
            Data::Solid(area) => {
                let len = (u32_at(area, off).ok_or_else(bad)? & 0x7FFF_FFFF) as usize;
                area.get(off + 4..off + 4 + len)
                    .map(<[u8]>::to_vec)
                    .ok_or_else(bad)
            }
            Data::Blocks { area, method } => {
                let n = u32_at(area, off).ok_or_else(bad)?;
                let len = (n & 0x7FFF_FFFF) as usize;
                let raw = area.get(off + 4..off + 4 + len).ok_or_else(bad)?;
                if n & 0x8000_0000 != 0 {
                    decompress(*method, raw, MAX_STREAM)
                } else {
                    Ok(raw.to_vec())
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StrKind {
    Unicode,
    /// NSIS 3 ANSI: codes 1..=4.
    Ansi3,
    /// NSIS 2 ANSI: codes 252..=255.
    Ansi2,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    Text(String),
    Var(u16),
    Shell(u8, u8),
    Lang(u16),
}

/// A parsed installer, ready to install.
pub(crate) struct Nsis<'a> {
    header: Vec<u8>,
    data: Data<'a>,
    strings: (usize, usize),
    kind: StrKind,
    entries: (usize, usize),
    lang: Vec<i32>,
    instdir: String,
    per_machine: bool,
    product: Product,
}

impl<'a> Nsis<'a> {
    /// `bytes` is the whole installer; `first_header` is where its NSIS
    /// first header starts.
    pub(crate) fn parse(
        bytes: &'a [u8],
        first_header: usize,
        w: &Writer,
    ) -> Result<Self, InstallError> {
        let fh = bytes
            .get(first_header..first_header + FIRST_HEADER_LEN)
            .ok_or_else(|| malformed("truncated first header"))?;
        if &fh[4..20] != b"\xEF\xBE\xAD\xDENullsoftInst" {
            return Err(malformed("bad first header"));
        }
        let header_len = u32_at(fh, 20).unwrap() as usize;
        let total = u32_at(fh, 24).unwrap() as usize;
        let body_start = first_header + FIRST_HEADER_LEN;
        let body_end = (first_header + total).min(bytes.len());
        let body = bytes
            .get(body_start..body_end)
            .ok_or_else(|| malformed("truncated body"))?;
        if header_len > MAX_STREAM {
            return Err(malformed("header too large"));
        }

        let (header, data) = Self::split(body, header_len)?;
        let block = |i: usize| -> (usize, usize) {
            (
                u32_at(&header, 4 + 8 * i).unwrap_or(0) as usize,
                u32_at(&header, 8 + 8 * i).unwrap_or(0) as usize,
            )
        };
        if header.len() < 4 + 8 * 8 {
            return Err(malformed("header too short"));
        }
        let (entries_off, entries_num) = block(BLOCK_ENTRIES);
        let (strings_off, _) = block(BLOCK_STRINGS);
        let (lang_off, lang_num) = block(BLOCK_LANGTABLES);
        // The string table runs to the next block that follows it.
        let strings_end = (0..8)
            .map(|i| block(i).0)
            .filter(|&o| o > strings_off)
            .min()
            .unwrap_or(header.len())
            .min(header.len());
        if entries_off + entries_num * ENTRY_LEN > header.len() || strings_off >= strings_end {
            return Err(malformed("header blocks out of range"));
        }
        let kind = string_kind(&header[strings_off..strings_end]);
        let lang = lang_table(&header, lang_off, lang_num, |i| block(i).0);

        let mut n = Nsis {
            header,
            data,
            strings: (strings_off, strings_end),
            kind,
            entries: (entries_off, entries_num),
            lang,
            instdir: String::new(),
            per_machine: true,
            product: Product::default(),
        };
        n.choose_instdir(w);
        n.product = n.find_product(w);
        Ok(n)
    }

    /// Work out the compression and return the header and the data source.
    fn split(body: &'a [u8], header_len: usize) -> Result<(Vec<u8>, Data<'a>), InstallError> {
        // Solid LZMA: the whole body is one stream.
        if lzma_start(body) {
            if let Ok(all) = decompress(Method::Lzma, body, MAX_STREAM) {
                if u32_at(&all, 0) == Some(header_len as u32) && all.len() >= 4 + header_len {
                    let header = all[4..4 + header_len].to_vec();
                    let area = all[4 + header_len..].to_vec();
                    return Ok((header, Data::Solid(area)));
                }
            }
        }
        // Non-solid: a size word (high bit = compressed) before each block.
        if let Some(n) = u32_at(body, 0) {
            let len = (n & 0x7FFF_FFFF) as usize;
            if let Some(raw) = body.get(4..4 + len) {
                let area = &body[4 + len..];
                if n & 0x8000_0000 == 0 && len == header_len {
                    return Ok((
                        raw.to_vec(),
                        Data::Blocks {
                            area,
                            method: Method::Stored,
                        },
                    ));
                }
                for method in [Method::Lzma, Method::Deflate] {
                    if method == Method::Lzma && !lzma_start(raw) {
                        continue;
                    }
                    if let Ok(h) = decompress(method, raw, header_len) {
                        if h.len() == header_len {
                            return Ok((h, Data::Blocks { area, method }));
                        }
                    }
                }
            }
        }
        // Solid deflate.
        if let Ok(all) = decompress(Method::Deflate, body, MAX_STREAM) {
            if u32_at(&all, 0) == Some(header_len as u32) && all.len() >= 4 + header_len {
                let header = all[4..4 + header_len].to_vec();
                let area = all[4 + header_len..].to_vec();
                return Ok((header, Data::Solid(area)));
            }
        }
        Err(InstallError::Unsupported {
            kind: "NSIS installer",
            reason: "it uses a compression AnyOS does not read yet (likely NSIS bzip2)".into(),
        })
    }

    fn entry(&self, i: usize) -> [i32; 7] {
        let base = self.entries.0 + i * ENTRY_LEN;
        std::array::from_fn(|k| i32_at(&self.header, base + 4 * k).unwrap_or(0))
    }

    fn entries(&self) -> impl Iterator<Item = [i32; 7]> + '_ {
        (0..self.entries.1).map(|i| self.entry(i))
    }

    /// The pieces of string `off` (a negative offset names a language string).
    fn pieces(&self, off: i32) -> Option<Vec<Piece>> {
        let off = if off < 0 {
            *self.lang.get((-(off + 1)) as usize)?
        } else {
            off
        };
        if off < 0 {
            return None;
        }
        let (start, end) = self.strings;
        let table = &self.header[start..end];
        let unit = if self.kind == StrKind::Unicode { 2 } else { 1 };
        let mut pos = off as usize * unit;
        let next = |pos: &mut usize| -> Option<u16> {
            let v = if unit == 2 {
                u16::from_le_bytes(table.get(*pos..*pos + 2)?.try_into().ok()?)
            } else {
                *table.get(*pos)? as u16
            };
            *pos += unit;
            Some(v)
        };
        let (lang, shell, var, skip) = match self.kind {
            StrKind::Unicode | StrKind::Ansi3 => (1, 2, 3, 4),
            StrKind::Ansi2 => (255, 254, 253, 252),
        };
        let arg = |pos: &mut usize| -> Option<(u8, u8)> {
            if unit == 2 {
                let v = next(pos)?;
                Some(((v & 0xFF) as u8, (v >> 8) as u8))
            } else {
                Some((next(pos)? as u8, next(pos)? as u8))
            }
        };
        let short = |(a, b): (u8, u8)| ((b as u16 & 0x7F) << 7) | (a as u16 & 0x7F);
        let mut out = Vec::new();
        let mut text = Vec::<u16>::new();
        let flush = |text: &mut Vec<u16>, out: &mut Vec<Piece>| {
            if !text.is_empty() {
                let s = if unit == 2 {
                    String::from_utf16_lossy(text)
                } else {
                    text.iter().map(|&c| c as u8 as char).collect()
                };
                out.push(Piece::Text(s));
                text.clear();
            }
        };
        loop {
            let c = next(&mut pos)?;
            if c == 0 {
                break;
            }
            if c == skip {
                text.push(next(&mut pos)?);
            } else if c == var {
                flush(&mut text, &mut out);
                out.push(Piece::Var(short(arg(&mut pos)?)));
            } else if c == shell {
                flush(&mut text, &mut out);
                let (a, b) = arg(&mut pos)?;
                out.push(Piece::Shell(a, b));
            } else if c == lang {
                flush(&mut text, &mut out);
                out.push(Piece::Lang(short(arg(&mut pos)?)));
            } else {
                text.push(c);
            }
        }
        flush(&mut text, &mut out);
        Some(out)
    }

    /// Plain text of a string with no variables (used for registry value names).
    fn plain(&self, off: i32) -> Option<String> {
        let mut s = String::new();
        for p in self.pieces(off)? {
            match p {
                Piece::Text(t) => s.push_str(&t),
                Piece::Lang(id) => s.push_str(&self.plain(-(id as i32) - 1)?),
                _ => return None,
            }
        }
        Some(s)
    }

    /// Expand string `off`. `None` when it uses something AnyOS cannot know
    /// without running the installer.
    fn eval(&self, off: i32, vars: &HashMap<u16, String>, w: &Writer) -> Option<String> {
        self.eval_depth(off, vars, w, 0)
    }

    fn eval_depth(
        &self,
        off: i32,
        vars: &HashMap<u16, String>,
        w: &Writer,
        depth: u8,
    ) -> Option<String> {
        if depth > 4 {
            return None;
        }
        let mut s = String::new();
        for p in self.pieces(off)? {
            match p {
                Piece::Text(t) => s.push_str(&t),
                Piece::Var(v) => s.push_str(&self.var(v, vars, w)?),
                Piece::Shell(a, b) => s.push_str(&self.shell(a, b, w)?),
                Piece::Lang(id) => {
                    s.push_str(&self.eval_depth(-(id as i32) - 1, vars, w, depth + 1)?)
                }
            }
        }
        Some(s)
    }

    fn var(&self, v: u16, vars: &HashMap<u16, String>, w: &Writer) -> Option<String> {
        match v {
            VAR_INSTDIR => Some(self.instdir.clone()),
            VAR_LANGUAGE => Some("1033".into()),
            VAR_TEMP => Some(w.env.known_folder(KnownFolder::Temp)),
            VAR_PLUGINSDIR => Some(format!(
                r"{}\nsis-plugins",
                w.env.known_folder(KnownFolder::Temp)
            )),
            20 => Some(String::new()), // $CMDLINE
            _ => vars.get(&v).cloned(),
        }
    }

    /// A shell folder. `a` is the per-user CSIDL and `b` the all-users one;
    /// a set high bit means a registry-backed folder such as Program Files.
    fn shell(&self, a: u8, b: u8, w: &Writer) -> Option<String> {
        use KnownFolder::*;
        if a & 0x80 != 0 {
            let name = self.plain((a & 0x3F) as i32)?;
            let x64 = a & 0x40 != 0;
            let f = match name.as_str() {
                "ProgramFilesDir" if x64 => ProgramFiles,
                "ProgramFilesDir" => ProgramFilesX86,
                "CommonFilesDir" if x64 => CommonFiles,
                "CommonFilesDir" => CommonFilesX86,
                _ => return None,
            };
            return Some(w.env.known_folder(f));
        }
        let csidl = if self.per_machine && b != 0 { b } else { a };
        let f = match csidl {
            0x00 | 0x10 => Desktop,
            0x02 => Programs,
            0x05 | 0x2E => Documents,
            0x07 | 0x18 => Startup,
            0x0B => StartMenu,
            0x14 => Fonts,
            0x16 => CommonStartMenu,
            0x17 => CommonPrograms,
            0x19 => CommonDesktop,
            0x1A => AppData,
            0x1C => LocalAppData,
            0x23 => ProgramData,
            0x24 => Windows,
            0x25 => SysWow64,
            0x26 => ProgramFilesX86,
            0x28 => Profile,
            0x2B => CommonFilesX86,
            _ => return None,
        };
        Some(w.env.known_folder(f))
    }

    /// `$INSTDIR`: a 64-bit Program Files choice the script makes, else the
    /// installer's default (the header's install directory), else the first
    /// folder the script assigns.
    fn choose_instdir(&mut self, w: &Writer) {
        let none = HashMap::new();
        let mut assigned = Vec::new();
        let entries: Vec<[i32; 7]> = self.entries().collect();
        for e in &entries {
            if e[0] == EW_ASSIGNVAR && e[1] == VAR_INSTDIR as i32 {
                if let Some(pieces) = self.pieces(e[2]) {
                    let x64 = pieces
                        .iter()
                        .any(|p| matches!(p, Piece::Shell(a, _) if a & 0xC0 == 0xC0));
                    let mut tmp = String::new();
                    std::mem::swap(&mut tmp, &mut self.instdir);
                    let value = self.eval(e[2], &none, w);
                    std::mem::swap(&mut tmp, &mut self.instdir);
                    if let Some(v) = value.filter(|v| v.len() > 3 && v.starts_with("C:")) {
                        assigned.push((x64, v));
                    }
                }
            }
        }
        let header_default = self.header_install_dir(w);
        self.instdir = assigned
            .iter()
            .rev()
            .find(|(x64, _)| *x64)
            .map(|(_, v)| v.clone())
            .or(header_default)
            .or_else(|| assigned.first().map(|(_, v)| v.clone()))
            .unwrap_or_default();
        let lower = self.instdir.to_ascii_lowercase();
        self.per_machine = lower.starts_with(r"c:\program files") || self.instdir.is_empty();
    }

    /// The header's `install_directory_ptr`: an int after the block table
    /// whose string is a path, followed by `install_directory_auto_append`,
    /// whose string is the path's last part.
    fn header_install_dir(&self, w: &Writer) -> Option<String> {
        let none = HashMap::new();
        let first_block = (0..8)
            .filter_map(|i| u32_at(&self.header, 4 + 8 * i).map(|o| o as usize))
            .filter(|&o| o > 0)
            .min()?;
        let ints = (4 + 64..first_block.min(4 + 64 + 4 * 200)).step_by(4);
        for off in ints {
            let (Some(p), Some(q)) = (i32_at(&self.header, off), i32_at(&self.header, off + 4))
            else {
                break;
            };
            if p <= 0 || q <= 0 {
                continue;
            }
            let (Some(path), Some(tail)) = (self.eval(p, &none, w), self.plain(q)) else {
                continue;
            };
            if !tail.is_empty() && path.starts_with("C:") && path.ends_with(&tail) {
                return Some(path);
            }
        }
        None
    }

    /// Name, version and publisher from the Add/Remove Programs entry the
    /// script writes, else the install folder's name.
    fn find_product(&self, w: &Writer) -> Product {
        let vars = HashMap::new();
        let mut p = Product::default();
        for e in self.entries() {
            if e[0] != EW_WRITEREG {
                continue;
            }
            let Some(key) = self.eval(e[2], &vars, w) else {
                continue;
            };
            if !key.to_ascii_lowercase().contains(r"\uninstall\") {
                continue;
            }
            let Some(name) = self.plain(e[3]) else {
                continue;
            };
            let value = self.eval(e[4], &vars, w).filter(|v| !v.trim().is_empty());
            match name.as_str() {
                "DisplayName" if p.name.is_empty() => p.name = value.unwrap_or_default(),
                "DisplayVersion" if p.version.is_none() => p.version = value,
                "Publisher" if p.publisher.is_none() => p.publisher = value,
                _ => {}
            }
        }
        if p.name.is_empty() {
            p.name = self
                .instdir
                .rsplit('\\')
                .next()
                .filter(|s| !s.is_empty() && !s.ends_with(':'))
                .unwrap_or_default()
                .to_string();
        }
        p
    }

    pub(crate) fn product(&self, stem: &str) -> Product {
        let mut p = self.product.clone();
        if p.name.is_empty() {
            p.name = stem.to_string();
        }
        p
    }

    pub(crate) fn install(
        &mut self,
        w: &mut Writer,
        product: &Product,
    ) -> Result<Installed, InstallError> {
        if self.instdir.is_empty() {
            self.instdir = anyos_env::win_join(
                &w.env.known_folder(KnownFolder::ProgramFiles),
                &product.name,
            );
            self.per_machine = true;
        }
        let temp = w.env.known_folder(KnownFolder::Temp).to_ascii_lowercase();
        let mut vars: HashMap<u16, String> = HashMap::new();
        let mut outdir: Option<String> = Some(self.instdir.clone());
        let mut installed = Installed {
            install_dir: Some(self.instdir.clone()),
            ..Installed::default()
        };
        let mut unresolved = 0;
        let mut plugin_calls = 0;
        let mut execs = 0;
        let mut cache: HashMap<i32, Vec<u8>> = HashMap::new();

        for e in self.entries().collect::<Vec<_>>() {
            match e[0] {
                EW_ASSIGNVAR => {
                    let v = e[1] as u16;
                    if v == VAR_INSTDIR {
                        continue;
                    }
                    // StrCpy with a length or start offset is not a plain copy.
                    match self.eval(e[2], &vars, w).filter(|_| e[3] == 0 && e[4] == 0) {
                        Some(value) => {
                            if v == VAR_OUTDIR {
                                outdir = Some(value.clone());
                            }
                            vars.insert(v, value);
                        }
                        None => {
                            vars.remove(&v);
                        }
                    }
                }
                EW_CREATEDIR => {
                    let path = self.eval(e[1], &vars, w).map(|p| trim_slash(&p));
                    if e[2] != 0 {
                        outdir = path.clone();
                        match &outdir {
                            Some(o) => vars.insert(VAR_OUTDIR, o.clone()),
                            None => vars.remove(&VAR_OUTDIR),
                        };
                    }
                    if let Some(p) = path {
                        if !p.to_ascii_lowercase().starts_with(&temp) && w.env.host_path(&p).is_ok()
                        {
                            w.mkdir(&p)?;
                        }
                    }
                }
                EW_EXTRACTFILE => {
                    let Some(name) = self.eval(e[2], &vars, w) else {
                        unresolved += 1;
                        continue;
                    };
                    let full = if name.len() > 1 && name.as_bytes()[1] == b':' {
                        name
                    } else {
                        match &outdir {
                            Some(o) => anyos_env::win_join(o, &name),
                            None => {
                                unresolved += 1;
                                continue;
                            }
                        }
                    };
                    if full.to_ascii_lowercase().starts_with(&temp) {
                        continue;
                    }
                    let data = match cache.get(&e[3]) {
                        Some(d) => d.clone(),
                        None => {
                            let d = self.data.block(e[3] as u32)?;
                            cache.insert(e[3], d.clone());
                            d
                        }
                    };
                    let (dir, file) = full.rsplit_once('\\').unwrap_or(("C:", &full));
                    match w.write(dir, file, &mut data.as_slice()) {
                        Ok(()) => {}
                        Err(InstallError::Env(_)) => w.note(format!("skipped unsafe path {full}")),
                        Err(e) => return Err(e),
                    }
                }
                EW_WRITEREG => {
                    let root = match e[1] as u32 {
                        0x8000_0000 => "HKCR",
                        0x8000_0001 => "HKCU",
                        0x8000_0002 => "HKLM",
                        0x8000_0003 => "HKU",
                        0 if self.per_machine => "HKLM",
                        0 => "HKCU",
                        _ => continue,
                    };
                    let (Some(key), Some(name)) =
                        (self.eval(e[2], &vars, w), self.eval(e[3], &vars, w))
                    else {
                        continue;
                    };
                    let key = format!("{root}\\{key}");
                    let value = match e[5] {
                        1 | 2 => self.eval(e[4], &vars, w).map(|v| {
                            if e[6] == 2 {
                                RegValue::ExpandSz(v)
                            } else {
                                RegValue::Sz(v)
                            }
                        }),
                        4 => self
                            .eval(e[4], &vars, w)
                            .and_then(|v| parse_int(&v))
                            .map(|n| RegValue::Dword(n as u32)),
                        3 => self.data.block(e[4] as u32).ok().map(RegValue::Binary),
                        _ => None,
                    };
                    if let Some(v) = value {
                        installed.registry.push((key, name, v));
                    }
                }
                EW_CREATESHORTCUT => {
                    let (Some(link), Some(target)) =
                        (self.eval(e[1], &vars, w), self.eval(e[2], &vars, w))
                    else {
                        continue;
                    };
                    let (folder, file) = link.rsplit_once('\\').unwrap_or(("", &link));
                    let name = file.strip_suffix(".lnk").unwrap_or(file).to_string();
                    installed.shortcuts.push(Shortcut {
                        name,
                        folder: folder.to_string(),
                        target,
                        arguments: self.eval(e[3], &vars, w).filter(|a| !a.is_empty()),
                        working_dir: outdir.clone(),
                    });
                }
                EW_REGISTERDLL => plugin_calls += 1,
                EW_EXECUTE => execs += 1,
                EW_WRITEUNINSTALLER => w.note("the installer's uninstaller was not created"),
                _ => {}
            }
        }
        if unresolved > 0 {
            w.note(format!(
                "skipped {unresolved} file(s) whose folder depends on choices made while the installer runs"
            ));
        }
        if plugin_calls > 0 {
            w.note(format!(
                "{plugin_calls} plugin call(s) in the script were not run"
            ));
        }
        if execs > 0 {
            w.note(format!("{execs} program(s) the script starts were not run"));
        }
        installed.main = installed
            .shortcuts
            .iter()
            .map(|s| s.target.clone())
            .find(|t| t.to_ascii_lowercase().ends_with(".exe") && !is_uninstaller(t));
        Ok(installed)
    }
}

fn is_uninstaller(path: &str) -> bool {
    let l = path.to_ascii_lowercase();
    l.contains("unins")
}

fn trim_slash(p: &str) -> String {
    let t = p.trim_end_matches('\\');
    if t.len() == 2 && t.ends_with(':') {
        format!("{t}\\")
    } else {
        t.to_string()
    }
}

fn parse_int(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return i64::from_str_radix(h, 16).ok();
    }
    s.parse().ok()
}

fn string_kind(table: &[u8]) -> StrKind {
    let sample = &table[..table.len().min(4096)];
    let odd_zeros = sample
        .iter()
        .skip(1)
        .step_by(2)
        .filter(|&&b| b == 0)
        .count();
    // The table starts with the empty string; UTF-16 text of mostly ASCII
    // has a zero in most odd bytes.
    if sample.starts_with(&[0, 0]) && odd_zeros * 10 > sample.len() / 2 * 6 {
        return StrKind::Unicode;
    }
    let new = table.iter().filter(|&&b| (1..=4).contains(&b)).count();
    let old = table.iter().filter(|&&b| b >= 252).count();
    if old > new {
        StrKind::Ansi2
    } else {
        StrKind::Ansi3
    }
}

/// String offsets of the English language table, else the first one.
fn lang_table(
    header: &[u8],
    off: usize,
    num: usize,
    block_off: impl Fn(usize) -> usize,
) -> Vec<i32> {
    if num == 0 || off == 0 {
        return Vec::new();
    }
    let end = (0..8)
        .map(&block_off)
        .filter(|&o| o > off)
        .min()
        .unwrap_or(header.len())
        .min(header.len());
    let size = (end.saturating_sub(off)) / num;
    if size < 10 {
        return Vec::new();
    }
    let table_at = |i: usize| off + i * size;
    let pick = (0..num)
        .find(|&i| {
            header
                .get(table_at(i)..table_at(i) + 2)
                .is_some_and(|b| u16::from_le_bytes([b[0], b[1]]) == 1033)
        })
        .unwrap_or(0);
    let start = table_at(pick) + 10;
    (0..(size - 10) / 4)
        .map(|k| i32_at(header, start + 4 * k).unwrap_or(-1))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_kinds() {
        let unicode: Vec<u8> = "\0abc\0def\0"
            .encode_utf16()
            .flat_map(|c| c.to_le_bytes())
            .collect();
        assert_eq!(string_kind(&unicode), StrKind::Unicode);
        assert_eq!(string_kind(b"\0abc\x03\x95\x80\\x\0"), StrKind::Ansi3);
        assert_eq!(string_kind(b"\0abc\xFD\x95\x80\\x\0"), StrKind::Ansi2);
    }

    #[test]
    fn ints() {
        assert_eq!(parse_int("0x10"), Some(16));
        assert_eq!(parse_int(" 42 "), Some(42));
        assert_eq!(parse_int("x"), None);
        assert_eq!(trim_slash(r"C:\a\"), r"C:\a");
        assert_eq!(trim_slash(r"C:\"), r"C:\");
    }
}
