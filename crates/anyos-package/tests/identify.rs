//! Each format is recognised from synthetic files carrying only its markers,
//! built on the committed fixtures, so no third-party installer is checked in.

use std::path::PathBuf;

use anyos_package::{identify, InstallRoute, PackageKind, PackageOs};

fn fixture(rel: &str) -> Vec<u8> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(rel);
    std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn pe_kind(bytes: &[u8]) -> Option<PackageKind> {
    let image = anyos_pe::parse(bytes).unwrap();
    identify(bytes, Some(&image)).map(|p| p.kind)
}

fn with_overlay(overlay: &[u8]) -> Vec<u8> {
    let mut b = fixture("bin/x64/hello_console.exe");
    b.extend_from_slice(overlay);
    b
}

#[test]
fn plain_programs_are_not_installers() {
    for f in ["bin/x64/hello_console.exe", "bin/x86/hello_gui.exe"] {
        assert_eq!(pe_kind(&fixture(f)), None, "{f}");
    }
    for f in ["bin/other/linux-x64-static", "bin/other/macos-arm64"] {
        assert_eq!(identify(&fixture(f), None), None, "{f}");
    }
}

#[test]
fn nsis_first_header_in_overlay() {
    let plain = fixture("bin/x64/hello_console.exe").len() as u64;
    let mut overlay = vec![0u8; 512];
    overlay.extend_from_slice(&0u32.to_le_bytes());
    overlay.extend_from_slice(b"\xEF\xBE\xAD\xDENullsoftInst");
    overlay.extend_from_slice(&[0u8; 64]);
    let b = with_overlay(&overlay);
    let image = anyos_pe::parse(&b).unwrap();
    let p = identify(&b, Some(&image)).unwrap();
    assert_eq!(p.kind, PackageKind::Nsis);
    assert_eq!(p.payload_offset, Some(plain + 512));
    assert_eq!(p.kind.os(), PackageOs::Windows);
    assert_eq!(p.kind.route(), InstallRoute::Extract);
    assert_eq!(p.kind.silent_args(), ["/S"]);
}

#[test]
fn inno_setup_markers() {
    assert_eq!(
        pe_kind(&with_overlay(b"rDlPtS02\x87eVx\0\0\0\0")),
        Some(PackageKind::InnoSetup)
    );
    assert_eq!(
        pe_kind(&with_overlay(b"Inno Setup Setup Data (6.2.0)")),
        Some(PackageKind::InnoSetup)
    );
}

#[test]
fn archive_self_extractors() {
    assert_eq!(
        pe_kind(&with_overlay(b"7z\xBC\xAF\x27\x1C\0\x04")),
        Some(PackageKind::SevenZipSfx)
    );
    assert_eq!(
        pe_kind(&with_overlay(b"PK\x03\x04\x14\0")),
        Some(PackageKind::ZipSfx)
    );
}

/// A compound file whose root storage carries `clsid`.
fn cfb(clsid: [u8; 16]) -> Vec<u8> {
    let mut b = vec![0u8; 1024];
    b[..8].copy_from_slice(b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1");
    b[0x1E..0x20].copy_from_slice(&9u16.to_le_bytes());
    b[0x30..0x34].copy_from_slice(&0u32.to_le_bytes());
    b[512 + 0x50..512 + 0x60].copy_from_slice(&clsid);
    b
}

fn msi_clsid(first: u8) -> [u8; 16] {
    [
        first, 0x10, 0x0C, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x46,
    ]
}

#[test]
fn windows_installer_databases() {
    let kind = |b: Vec<u8>| identify(&b, None).map(|p| p.kind);
    assert_eq!(kind(cfb(msi_clsid(0x84))), Some(PackageKind::Msi));
    assert_eq!(kind(cfb(msi_clsid(0x86))), Some(PackageKind::MsiPatch));
    assert_eq!(kind(cfb(msi_clsid(0x82))), Some(PackageKind::MsiTransform));
    // A Word 97 document is a compound file too, with another root CLSID.
    let mut word = msi_clsid(0x06);
    word[1] = 0x09;
    word[2] = 0x02;
    assert_eq!(kind(cfb(word)), None);
    // Truncated header.
    assert_eq!(kind(cfb(msi_clsid(0x84))[..300].to_vec()), None);
}

#[test]
fn linux_and_macos_packages() {
    let kind = |b: &[u8]| identify(b, None).map(|p| p.kind);
    assert_eq!(kind(b"!<arch>\ndebian-binary   0"), Some(PackageKind::Deb));
    assert_eq!(kind(b"\xED\xAB\xEE\xDB\x03\x00"), Some(PackageKind::Rpm));
    assert_eq!(kind(b"xar!\x00\x1c"), Some(PackageKind::MacPkg));
    assert_eq!(kind(b"PK\x03\x04\x14\0"), Some(PackageKind::Zip));
    assert_eq!(
        kind(b"7z\xBC\xAF\x27\x1C\0\x04"),
        Some(PackageKind::SevenZip)
    );

    let mut dmg = vec![0u8; 4096];
    let n = dmg.len();
    dmg[n - 512..n - 508].copy_from_slice(b"koly");
    assert_eq!(kind(&dmg), Some(PackageKind::Dmg));

    let mut appimage = fixture("bin/other/linux-x64-static");
    appimage[8..11].copy_from_slice(b"AI\x02");
    assert_eq!(kind(&appimage), Some(PackageKind::AppImage));
    assert_eq!(PackageKind::AppImage.os(), PackageOs::Linux);
}
