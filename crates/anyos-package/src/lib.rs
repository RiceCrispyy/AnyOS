//! # anyos-package
//!
//! Recognises installers and app packages so AnyOS can decide how to install
//! an app into its own environment instead of onto the host.
//!
//! * Windows: NSIS, Inno Setup, WiX Burn bundles, InstallShield, 7-Zip and ZIP
//!   self-extractors (all `.exe`), and Windows Installer databases (`.msi`,
//!   `.msp`, `.mst`).
//! * Linux: `.deb`, `.rpm`, AppImage.
//! * macOS: flat installer packages (`.pkg`) and disk images (`.dmg`).
//!
//! Detection only looks for each format's documented markers. Nothing here
//! executes code or unpacks a payload.

use anyos_pe::PeImage;
use serde::Serialize;

/// How far into a PE overlay to look for an installer's own header.
const OVERLAY_SCAN: usize = 1 << 20;
/// How far into a file to look for markers that live in resources.
const RESOURCE_SCAN: usize = 4 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PackageKind {
    /// Nullsoft Scriptable Install System (Steam's installer uses it).
    Nsis,
    InnoSetup,
    /// WiX Toolset bootstrapper that chains MSI and EXE packages.
    WixBurn,
    InstallShield,
    SevenZipSfx,
    ZipSfx,
    /// Windows Installer database (`.msi`).
    Msi,
    /// Windows Installer patch (`.msp`).
    MsiPatch,
    /// Windows Installer transform (`.mst`).
    MsiTransform,
    Deb,
    Rpm,
    AppImage,
    /// macOS flat installer package (a xar archive).
    MacPkg,
    /// macOS disk image (UDIF).
    Dmg,
    /// A plain ZIP archive, treated as a portable Windows app.
    Zip,
    /// A plain 7-Zip archive, treated as a portable Windows app.
    SevenZip,
}

/// Which OS the package installs software for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PackageOs {
    Windows,
    Linux,
    MacOs,
}

/// How AnyOS can get the app out of the package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum InstallRoute {
    /// The payload format is documented and can be unpacked by AnyOS directly,
    /// without running any guest code. Installer scripts, registry writes and
    /// custom actions are not run on this route.
    Extract,
    /// The package is a program that has to run as a guest to install
    /// (its payload format is private or depends on its own code).
    RunInstaller,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Package {
    pub kind: PackageKind,
    /// File offset of the installer's own data, when it was found in a PE overlay.
    pub payload_offset: Option<u64>,
}

impl PackageKind {
    pub fn display_name(self) -> &'static str {
        match self {
            PackageKind::Nsis => "NSIS installer",
            PackageKind::InnoSetup => "Inno Setup installer",
            PackageKind::WixBurn => "WiX Burn bundle",
            PackageKind::InstallShield => "InstallShield installer",
            PackageKind::SevenZipSfx => "7-Zip self-extractor",
            PackageKind::ZipSfx => "ZIP self-extractor",
            PackageKind::Msi => "Windows Installer package (MSI)",
            PackageKind::MsiPatch => "Windows Installer patch (MSP)",
            PackageKind::MsiTransform => "Windows Installer transform (MST)",
            PackageKind::Deb => "Debian package",
            PackageKind::Rpm => "RPM package",
            PackageKind::AppImage => "AppImage",
            PackageKind::MacPkg => "macOS installer package",
            PackageKind::Dmg => "macOS disk image",
            PackageKind::Zip => "ZIP archive",
            PackageKind::SevenZip => "7-Zip archive",
        }
    }

    pub fn os(self) -> PackageOs {
        match self {
            PackageKind::Deb | PackageKind::Rpm | PackageKind::AppImage => PackageOs::Linux,
            PackageKind::MacPkg | PackageKind::Dmg => PackageOs::MacOs,
            _ => PackageOs::Windows,
        }
    }

    /// The route AnyOS will take first. Formats with a documented payload are
    /// extracted; the rest must run.
    pub fn route(self) -> InstallRoute {
        match self {
            PackageKind::WixBurn | PackageKind::InstallShield => InstallRoute::RunInstaller,
            _ => InstallRoute::Extract,
        }
    }

    /// Arguments that make the installer run without its wizard UI, which lets
    /// AnyOS run it before the Win32 GUI layer exists.
    pub fn silent_args(self) -> &'static [&'static str] {
        match self {
            PackageKind::Nsis => &["/S"],
            PackageKind::InnoSetup => &["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"],
            PackageKind::WixBurn => &["/quiet", "/norestart"],
            PackageKind::InstallShield => &["/s"],
            PackageKind::SevenZipSfx => &["-y"],
            PackageKind::Msi | PackageKind::MsiPatch => &["/qn", "/norestart"],
            _ => &[],
        }
    }
}

impl PackageOs {
    pub fn display_name(self) -> &'static str {
        match self {
            PackageOs::Windows => "Windows",
            PackageOs::Linux => "Linux",
            PackageOs::MacOs => "macOS",
        }
    }
}

/// Identify an installer or package. `pe` is the parsed image when `bytes` is a
/// PE file, so its sections and overlay can be checked.
pub fn identify(bytes: &[u8], pe: Option<&PeImage>) -> Option<Package> {
    match pe {
        Some(image) => identify_pe(bytes, image),
        None => identify_other(bytes),
    }
}

fn identify_pe(bytes: &[u8], image: &PeImage) -> Option<Package> {
    let found = |kind| {
        Some(Package {
            kind,
            payload_offset: None,
        })
    };
    if image.sections.iter().any(|s| s.name == ".wixburn") {
        return found(PackageKind::WixBurn);
    }
    let overlay_start = (bytes.len() as u64).saturating_sub(image.overlay_size) as usize;
    let overlay = &bytes[overlay_start..];
    let head = &overlay[..overlay.len().min(OVERLAY_SCAN)];
    let in_overlay = |kind, pos: usize| {
        Some(Package {
            kind,
            payload_offset: Some((overlay_start + pos) as u64),
        })
    };

    // NSIS "firstheader": flags (u32), 0xDEADBEEF, "NullsoftInst", aligned to 512.
    if let Some(pos) = find(head, b"\xEF\xBE\xAD\xDENullsoftInst") {
        if pos >= 4 {
            return in_overlay(PackageKind::Nsis, pos - 4);
        }
    }
    // Inno Setup: the setup loader's offset table ("rDlPtS", in resources since
    // 5.1.5, at file offset 0x30 before that) or the setup data header.
    let early = &bytes[..bytes.len().min(RESOURCE_SCAN)];
    if find(early, b"rDlPtS").is_some() || find(head, b"Inno Setup Setup Data (").is_some() {
        return found(PackageKind::InnoSetup);
    }
    if let Some(pos) = find(head, b"7z\xBC\xAF\x27\x1C") {
        return in_overlay(PackageKind::SevenZipSfx, pos);
    }
    if find(head, b"InstallShield").is_some() || find(early, b"InstallShield").is_some() {
        return found(PackageKind::InstallShield);
    }
    if let Some(pos) = find(head, b"PK\x03\x04") {
        return in_overlay(PackageKind::ZipSfx, pos);
    }
    None
}

fn identify_other(bytes: &[u8]) -> Option<Package> {
    let found = |kind| {
        Some(Package {
            kind,
            payload_offset: None,
        })
    };
    if bytes.starts_with(b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1") {
        return cfb_root_clsid(bytes)
            .and_then(|c| msi_kind(&c))
            .and_then(found);
    }
    if bytes.starts_with(b"!<arch>\ndebian-binary") {
        return found(PackageKind::Deb);
    }
    if bytes.starts_with(b"\xED\xAB\xEE\xDB") {
        return found(PackageKind::Rpm);
    }
    // AppImage type 1 and 2 mark the ELF identification padding with "AI".
    if bytes.starts_with(b"\x7fELF") && matches!(bytes.get(8..11), Some(b"AI\x01" | b"AI\x02")) {
        return found(PackageKind::AppImage);
    }
    if bytes.starts_with(b"xar!") {
        return found(PackageKind::MacPkg);
    }
    if bytes.starts_with(b"PK\x03\x04") {
        return found(PackageKind::Zip);
    }
    if bytes.starts_with(b"7z\xBC\xAF\x27\x1C") {
        return found(PackageKind::SevenZip);
    }
    // UDIF disk images end with a 512-byte "koly" trailer.
    if bytes.len() >= 512 && bytes[bytes.len() - 512..].starts_with(b"koly") {
        return found(PackageKind::Dmg);
    }
    None
}

/// CLSID of the root storage of a compound file (MS-CFB), as stored on disk.
fn cfb_root_clsid(bytes: &[u8]) -> Option<[u8; 16]> {
    let shift = u16::from_le_bytes(bytes.get(0x1E..0x20)?.try_into().ok()?);
    if !(9..=16).contains(&shift) {
        return None;
    }
    let dir_sector = u32::from_le_bytes(bytes.get(0x30..0x34)?.try_into().ok()?);
    let root = (dir_sector as usize)
        .checked_add(1)?
        .checked_shl(shift as u32)?;
    // A directory entry's CLSID is at offset 0x50.
    bytes.get(root + 0x50..root + 0x60)?.try_into().ok()
}

fn msi_kind(clsid: &[u8; 16]) -> Option<PackageKind> {
    // {000C108x-0000-0000-C000-000000000046}, little-endian on disk.
    const REST: [u8; 15] = [
        0x10, 0x0C, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46,
    ];
    if clsid[1..] != REST {
        return None;
    }
    match clsid[0] {
        0x84 => Some(PackageKind::Msi),
        0x86 => Some(PackageKind::MsiPatch),
        0x82 => Some(PackageKind::MsiTransform),
        _ => None,
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_works_at_edges() {
        assert_eq!(find(b"abcdef", b"ab"), Some(0));
        assert_eq!(find(b"abcdef", b"ef"), Some(4));
        assert_eq!(find(b"abcdef", b"eg"), None);
        assert_eq!(find(b"a", b"ab"), None);
    }
}
