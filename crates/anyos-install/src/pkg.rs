//! macOS flat installer package (`.pkg`) installation.
//!
//! A flat `.pkg` is a XAR archive that contains one or more component packages.
//! Each component package is a directory entry containing:
//! * `PackageInfo` (XML, has the identifier, version and install-location)
//! * `Payload` (a CPIO archive compressed with gzip or xz)
//! * `Bom` (bill of materials, optional)
//!
//! AnyOS extracts every `Payload` archive and maps the macOS filesystem
//! layout into the environment:
//! * `/Applications/Foo.app/Contents/MacOS/foo` → `C:\Applications\Foo.app\...`
//! * everything else under `C:\` mirroring the macOS path
//!
//! The product name comes from the first component's `PackageInfo` `identifier`
//! attribute (last path component), its `version`, and the bundle display name
//! when present.

use std::io::{self, Read};

use crate::{InstallError, Installed, Product, Writer};

fn malformed(msg: impl Into<String>) -> InstallError {
    InstallError::Malformed {
        what: "macOS package",
        message: msg.into(),
    }
}

/// Map a macOS path to a Windows path under `C:\`.
fn mac_to_win(path: &str) -> String {
    let rel = path.trim_start_matches('/').trim_start_matches("./");
    let rel = rel.replace('/', "\\");
    if rel.is_empty() {
        "C:\\".into()
    } else {
        format!("C:\\{rel}")
    }
}

/// Extract a gzip-compressed CPIO archive.
fn extract_cpio_gz(data: &[u8], w: &mut Writer) -> Result<usize, InstallError> {
    let gz = flate2::read::GzDecoder::new(data);
    extract_cpio(gz, w)
}

/// Extract a CPIO archive (newc / odc format) from a reader.
fn extract_cpio<R: Read>(mut src: R, w: &mut Writer) -> Result<usize, InstallError> {
    let mut count = 0;
    loop {
        // newc header is 110 bytes
        let mut hdr = [0u8; 110];
        match src.read_exact(&mut hdr) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(malformed(format!("CPIO header: {e}"))),
        }
        let magic = &hdr[0..6];
        if magic != b"070701" && magic != b"070702" {
            // Not newc format; stop gracefully.
            break;
        }
        let namesize = hex4(&hdr[94..102])? as usize;
        let filesize = hex4(&hdr[54..62])? as u64;
        let mode = hex4(&hdr[14..22])?;

        // Read filename.
        let mut name_buf = vec![0u8; namesize];
        src.read_exact(&mut name_buf)
            .map_err(|e| malformed(format!("CPIO name: {e}")))?;
        let name = std::str::from_utf8(&name_buf)
            .unwrap_or("")
            .trim_end_matches('\0')
            .to_string();
        // Align to 4-byte boundary after header + name.
        let align_name = (110 + namesize).next_multiple_of(4) - (110 + namesize);
        io::copy(&mut (&mut src).take(align_name as u64), &mut io::sink())
            .map_err(|e| malformed(e.to_string()))?;

        if name == "TRAILER!!!" {
            // Drain file data (should be 0) and finish.
            io::copy(&mut (&mut src).take(filesize), &mut io::sink())
                .map_err(|e| malformed(e.to_string()))?;
            break;
        }

        let is_regular = (mode & 0o170000) == 0o100000;
        if is_regular && filesize > 0 {
            let win = mac_to_win(&name);
            let (dir, file) = win.rsplit_once('\\').unwrap_or(("C:", &win));
            if !file.is_empty() {
                let mut limited = (&mut src).take(filesize);
                match w.write(dir, file, &mut limited) {
                    Ok(()) => count += 1,
                    Err(InstallError::Env(_)) => {
                        w.note(format!("skipped unsafe path {win}"));
                        // Drain remaining bytes.
                        io::copy(&mut limited, &mut io::sink())
                            .map_err(|e| malformed(e.to_string()))?;
                    }
                    Err(e) => return Err(e),
                }
            } else {
                io::copy(&mut (&mut src).take(filesize), &mut io::sink())
                    .map_err(|e| malformed(e.to_string()))?;
            }
        } else {
            io::copy(&mut (&mut src).take(filesize), &mut io::sink())
                .map_err(|e| malformed(e.to_string()))?;
        }

        // Align to 4-byte boundary after file data.
        let align_data = (filesize as usize).next_multiple_of(4) - filesize as usize;
        io::copy(&mut (&mut src).take(align_data as u64), &mut io::sink())
            .map_err(|e| malformed(e.to_string()))?;
    }
    Ok(count)
}

fn hex4(b: &[u8]) -> Result<u32, InstallError> {
    let s = std::str::from_utf8(b).map_err(|_| malformed("bad CPIO hex field"))?;
    u32::from_str_radix(s.trim(), 16).map_err(|_| malformed(format!("bad CPIO hex: {s}")))
}

/// Parse `identifier` and `version` from a PackageInfo XML fragment.
fn parse_package_info(xml: &str) -> (Option<String>, Option<String>) {
    // We just look for the attributes without a full XML parser.
    let id = attr_value(xml, "identifier").map(|s| {
        // Use the last path component as a readable name.
        s.split('.').next_back().unwrap_or(s).to_string()
    });
    let version = attr_value(xml, "version").map(String::from);
    (id, version)
}

fn attr_value<'a>(xml: &'a str, attr: &str) -> Option<&'a str> {
    let needle = format!("{attr}=\"");
    let start = xml.find(&needle)? + needle.len();
    let end = xml[start..].find('"')?;
    Some(&xml[start..start + end])
}

pub(crate) fn product(bytes: &[u8], stem: &str) -> Result<Product, InstallError> {
    let cursor = io::Cursor::new(bytes);
    let mut pkg = xara::PkgReader::open(cursor).map_err(|e| malformed(format!("XAR: {e}")))?;
    let components = pkg.components();
    let mut p = Product::default();
    for component in &components {
        if let Ok(Some(xml)) = pkg.package_info(component) {
            let (id, version) = parse_package_info(&xml);
            if p.name.is_empty() {
                if let Some(n) = id {
                    p.name = n;
                }
            }
            if p.version.is_none() {
                p.version = version;
            }
        }
    }
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
    let cursor = io::Cursor::new(bytes);
    let mut pkg = xara::PkgReader::open(cursor).map_err(|e| malformed(format!("XAR: {e}")))?;
    let components = pkg.components();

    let mut install_location: Option<String> = None;
    let mut total = 0usize;

    for component in &components {
        if let Ok(Some(xml)) = pkg.package_info(component) {
            if install_location.is_none() {
                install_location = attr_value(&xml, "install-location")
                    .filter(|s| !s.is_empty())
                    .map(String::from);
            }
        }
        if let Ok(data) = pkg.payload(component) {
            match extract_cpio_gz(&data, w) {
                Ok(n) => total += n,
                Err(e) => w.note(format!("skipped payload ({e})")),
            }
        }
    }

    let install_dir = install_location
        .as_deref()
        .map(mac_to_win)
        .or_else(|| Some(format!("C:\\Applications\\{}", product.name)));
    let main = crate::guess_main(&w.files, &product.name);
    if total == 0 {
        w.note("no files were extracted from any payload");
    }
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
    fn path_mapping() {
        assert_eq!(
            mac_to_win("/Applications/Foo.app"),
            r"C:\Applications\Foo.app"
        );
        assert_eq!(mac_to_win("./usr/bin/foo"), r"C:\usr\bin\foo");
    }

    #[test]
    fn package_info_parse() {
        let xml = r#"<pkg-info identifier="com.example.MyApp" version="3.2.1" install-location="/Applications">"#;
        let (id, ver) = parse_package_info(xml);
        assert_eq!(id.as_deref(), Some("MyApp"));
        assert_eq!(ver.as_deref(), Some("3.2.1"));
    }
}
