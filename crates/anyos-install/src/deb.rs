//! Debian package (`.deb`) installation.
//!
//! A `.deb` is an `ar` archive that contains a `debian-binary` text file,
//! a `control.tar.*` member (metadata) and a `data.tar.*` member (files).
//! AnyOS extracts the `data.tar.*` member and maps the Linux filesystem
//! layout into the environment:
//!
//! * `/usr/bin/*` → `C:\usr\bin\`
//! * `/usr/lib/*` → `C:\usr\lib\`
//! * everything else follows the same path under `C:\`
//!
//! The `control.tar.*` member's `./control` file carries the `Package`,
//! `Version` and `Maintainer` fields used as the product name, version and
//! publisher.
//!
//! Supported data-archive compression: gzip, xz, zstd, plain (`.tar`).
//! A bzip2-compressed data archive is refused.

use std::io::Read;

use crate::{InstallError, Installed, Product, Writer};

fn malformed(msg: impl Into<String>) -> InstallError {
    InstallError::Malformed {
        what: "Debian package",
        message: msg.into(),
    }
}

/// Parse the control file's fields into a Product.
fn parse_control(text: &str) -> Product {
    let mut p = Product::default();
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim();
        match k.trim() {
            "Package" if p.name.is_empty() => p.name = v.to_string(),
            "Version" if p.version.is_none() => p.version = Some(v.to_string()),
            "Maintainer" if p.publisher.is_none() => {
                // Strip email in angle-brackets: "John Doe <j@x.org>" → "John Doe"
                let name = v.split('<').next().unwrap_or(v).trim();
                if !name.is_empty() {
                    p.publisher = Some(name.to_string());
                }
            }
            _ => {}
        }
    }
    p
}

/// Map a Linux path (e.g. `/usr/bin/foo`) to a Windows path under `C:\`.
fn linux_to_win(path: &str) -> String {
    let rel = path.trim_start_matches('/').trim_start_matches("./");
    let rel = rel.replace('/', "\\");
    if rel.is_empty() {
        "C:\\".into()
    } else {
        format!("C:\\{rel}")
    }
}

/// Extract a tar archive (already decompressed) into the environment.
fn extract_tar<R: Read>(src: R, w: &mut Writer) -> Result<usize, InstallError> {
    let mut archive = tar::Archive::new(src);
    let mut count = 0;
    for entry in archive.entries().map_err(|e| malformed(e.to_string()))? {
        let mut entry = entry.map_err(|e| malformed(e.to_string()))?;
        let header = entry.header();
        if header.entry_type().is_dir() {
            continue;
        }
        if !header.entry_type().is_file() {
            continue;
        }
        let path = entry.path().map_err(|e| malformed(e.to_string()))?;
        let path_str = path.to_string_lossy().into_owned();
        // Skip package metadata inside the data archive (./.)
        if path_str == "./." || path_str == "." {
            continue;
        }
        let win = linux_to_win(&path_str);
        let (dir, file) = win.rsplit_once('\\').unwrap_or(("C:", &win));
        if file.is_empty() {
            continue;
        }
        match w.write(dir, file, &mut entry) {
            Ok(()) => count += 1,
            Err(InstallError::Env(_)) => {
                w.note(format!("skipped unsafe path {win}"));
            }
            Err(e) => return Err(e),
        }
    }
    Ok(count)
}

/// Decompress and extract the `data.tar.*` from the `ar` bytes.  Returns
/// the number of files extracted.
fn extract_data(ar_bytes: &[u8], w: &mut Writer) -> Result<usize, InstallError> {
    let mut offset = 8usize; // skip "!<arch>\n"
    while offset + 60 <= ar_bytes.len() {
        let header = &ar_bytes[offset..offset + 60];
        let name = std::str::from_utf8(&header[0..16]).unwrap_or("").trim_end();
        let size: usize = std::str::from_utf8(&header[48..58])
            .unwrap_or("0")
            .trim()
            .parse()
            .unwrap_or(0);
        let magic = &header[58..60];
        if magic != b"\x60\x0A" {
            return Err(malformed("bad ar member magic"));
        }
        offset += 60;
        let member = ar_bytes
            .get(offset..offset + size)
            .ok_or_else(|| malformed(format!("ar member {name} is truncated")))?;

        if name.starts_with("data.tar") {
            let count = if name.ends_with(".gz") || name.ends_with(".tgz") {
                let gz = flate2::read::GzDecoder::new(member);
                extract_tar(gz, w)?
            } else if name.ends_with(".xz") || name.ends_with(".lzma") {
                let xz = xz2::read::XzDecoder::new(member);
                extract_tar(xz, w)?
            } else if name.ends_with(".zst") {
                let zst =
                    zstd::Decoder::new(member).map_err(|e| malformed(format!("zstd: {e}")))?;
                extract_tar(zst, w)?
            } else if name == "data.tar" {
                extract_tar(member, w)?
            } else {
                return Err(InstallError::Unsupported {
                    kind: "Debian package",
                    reason: format!("data archive uses unsupported compression ({name})"),
                });
            };
            return Ok(count);
        }
        // Advance past this member, aligned to even offset.
        offset += size;
        if !size.is_multiple_of(2) {
            offset += 1;
        }
    }
    Err(malformed("no data.tar.* member found"))
}

/// Read the control file from the `control.tar.*` member.
fn read_control(ar_bytes: &[u8]) -> Product {
    let mut offset = 8usize;
    while offset + 60 <= ar_bytes.len() {
        let header = &ar_bytes[offset..offset + 60];
        let name = std::str::from_utf8(&header[0..16]).unwrap_or("").trim_end();
        let size: usize = std::str::from_utf8(&header[48..58])
            .unwrap_or("0")
            .trim()
            .parse()
            .unwrap_or(0);
        offset += 60;
        let member = match ar_bytes.get(offset..offset + size) {
            Some(m) => m,
            None => break,
        };

        if name.starts_with("control.tar") {
            let text = extract_control_text(name, member);
            if let Some(t) = text {
                return parse_control(&t);
            }
            break;
        }
        offset += size;
        if !size.is_multiple_of(2) {
            offset += 1;
        }
    }
    Product::default()
}

fn extract_control_text(name: &str, data: &[u8]) -> Option<String> {
    let text = if name.ends_with(".gz") || name.ends_with(".tgz") {
        let gz = flate2::read::GzDecoder::new(data);
        read_control_from_tar(gz)
    } else if name.ends_with(".xz") {
        let xz = xz2::read::XzDecoder::new(data);
        read_control_from_tar(xz)
    } else if name.ends_with(".zst") {
        let zst = zstd::Decoder::new(data).ok()?;
        read_control_from_tar(zst)
    } else if name == "control.tar" {
        read_control_from_tar(data)
    } else {
        return None;
    };
    text.ok()
}

fn read_control_from_tar<R: Read>(src: R) -> Result<String, ()> {
    let mut archive = tar::Archive::new(src);
    let entries = archive.entries().map_err(|_| ())?;
    for entry in entries {
        let mut entry = entry.map_err(|_| ())?;
        let path = entry.path().map_err(|_| ())?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name == "control" {
            let mut text = String::new();
            entry.read_to_string(&mut text).map_err(|_| ())?;
            return Ok(text);
        }
    }
    Err(())
}

pub(crate) fn product(bytes: &[u8], stem: &str) -> Result<Product, InstallError> {
    let mut p = read_control(bytes);
    if p.name.is_empty() {
        p.name = stem.to_string();
    }
    Ok(p)
}

pub(crate) fn install(
    w: &mut Writer,
    bytes: &[u8],
    product: &Product,
) -> Result<Installed, InstallError> {
    let _count = extract_data(bytes, w)?;
    let install_dir = Some(linux_to_win("/usr"));
    let main = crate::guess_main(&w.files, &product.name);
    Ok(Installed {
        install_dir,
        main,
        ..Installed::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_parse() {
        let p = parse_control(
            "Package: my-app\nVersion: 2.1.0\nMaintainer: Dev Team <dev@example.com>\n",
        );
        assert_eq!(p.name, "my-app");
        assert_eq!(p.version.as_deref(), Some("2.1.0"));
        assert_eq!(p.publisher.as_deref(), Some("Dev Team"));
    }

    #[test]
    fn path_mapping() {
        assert_eq!(linux_to_win("/usr/bin/tool"), r"C:\usr\bin\tool");
        assert_eq!(linux_to_win("./usr/share/doc"), r"C:\usr\share\doc");
        assert_eq!(linux_to_win("/"), "C:\\");
    }
}
