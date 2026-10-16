//! Bounds-checked access to file bytes by RVA.
//!
//! All directory parsing in this crate goes through [`ImageView`]: every read is
//! checked against both the section table and the end of the file, so malformed
//! input produces a [`PeError`] instead of a panic or an out-of-range read.

use crate::model::Section;
use crate::PeError;

pub(crate) struct ImageView<'a> {
    bytes: &'a [u8],
    sections: &'a [Section],
    size_of_headers: u32,
    file_alignment: u32,
}

impl<'a> ImageView<'a> {
    pub fn new(
        bytes: &'a [u8],
        sections: &'a [Section],
        size_of_headers: u32,
        file_alignment: u32,
    ) -> Self {
        ImageView {
            bytes,
            sections,
            size_of_headers,
            file_alignment,
        }
    }

    /// Windows rounds `PointerToRawData` down to 512 bytes when the file alignment
    /// is at least that large; matching it keeps us consistent with the real loader.
    pub fn raw_pointer(&self, s: &Section) -> u32 {
        if self.file_alignment >= 0x200 {
            s.raw_offset & !0x1ff
        } else {
            s.raw_offset
        }
    }

    /// Translate an RVA to a file offset plus the number of file-backed bytes
    /// available from there within the same section (or header region).
    pub fn rva_to_offset(&self, rva: u32) -> Option<(usize, usize)> {
        if rva < self.size_of_headers {
            let off = rva as usize;
            let end = (self.size_of_headers as usize).min(self.bytes.len());
            return (off < end).then(|| (off, end - off));
        }
        for s in self.sections {
            let start = s.virtual_address;
            let span = s.mapped_size().max(s.raw_size);
            if rva >= start && (rva as u64) < start as u64 + span as u64 {
                let delta = rva - start;
                if delta >= s.raw_size {
                    return None; // zero-fill area: not backed by file bytes
                }
                let off = self.raw_pointer(s) as usize + delta as usize;
                let section_end =
                    (self.raw_pointer(s) as usize).saturating_add(s.raw_size as usize);
                let end = section_end.min(self.bytes.len());
                return (off < end).then(|| (off, end - off));
            }
        }
        None
    }

    pub fn slice(&self, rva: u32, len: usize) -> Result<&'a [u8], PeError> {
        let (off, avail) = self.rva_to_offset(rva).ok_or_else(|| {
            PeError::Malformed(format!("RVA 0x{rva:x} is not backed by file data"))
        })?;
        if len > avail {
            return Err(PeError::Malformed(format!(
                "read of {len} bytes at RVA 0x{rva:x} runs past its section"
            )));
        }
        Ok(&self.bytes[off..off + len])
    }

    pub fn u16(&self, rva: u32) -> Result<u16, PeError> {
        let b = self.slice(rva, 2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn u32(&self, rva: u32) -> Result<u32, PeError> {
        let b = self.slice(rva, 4)?;
        Ok(u32::from_le_bytes(b.try_into().expect("4 bytes")))
    }

    pub fn u64(&self, rva: u32) -> Result<u64, PeError> {
        let b = self.slice(rva, 8)?;
        Ok(u64::from_le_bytes(b.try_into().expect("8 bytes")))
    }

    /// Pointer-sized little-endian value.
    pub fn ptr(&self, rva: u32, size: usize) -> Result<u64, PeError> {
        if size == 8 {
            self.u64(rva)
        } else {
            self.u32(rva).map(u64::from)
        }
    }

    /// NUL-terminated string of at most `max` bytes. Non-UTF-8 bytes are replaced.
    pub fn cstr(&self, rva: u32, max: usize) -> Result<String, PeError> {
        let (off, avail) = self.rva_to_offset(rva).ok_or_else(|| {
            PeError::Malformed(format!("string RVA 0x{rva:x} is not backed by file data"))
        })?;
        let window = &self.bytes[off..off + avail.min(max)];
        match window.iter().position(|&b| b == 0) {
            Some(n) => Ok(String::from_utf8_lossy(&window[..n]).into_owned()),
            None => Err(PeError::Malformed(format!(
                "unterminated string at RVA 0x{rva:x}"
            ))),
        }
    }
}

/// `rva + n` without wrapping.
pub(crate) fn add(rva: u32, n: u64) -> Result<u32, PeError> {
    u32::try_from(rva as u64 + n)
        .map_err(|_| PeError::Malformed(format!("RVA overflow at 0x{rva:x}")))
}
