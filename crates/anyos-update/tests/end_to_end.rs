//! Downloads a release from a local HTTP server and installs it into a
//! temporary folder, the way `anyos update` does against GitHub.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyos_update::{install_into, parse_latest, Error, Layout, Version};
use sha2::{Digest, Sha256};

const EXE: &str = std::env::consts::EXE_SUFFIX;
const TARGET: &str = "x86_64-unknown-linux-gnu";

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("anyos-update-e2e-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// A `.tar.gz` shaped like the release workflow's: one `anyos-<tag>-<target>/` folder.
fn release_archive(tag: &str) -> Vec<u8> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut tar = tar::Builder::new(gz);
    let top = format!("anyos-{tag}-{TARGET}");
    for (name, body, mode) in [
        (format!("anyos{EXE}"), "new cli", 0o755),
        (format!("anyos-gui{EXE}"), "new gui", 0o755),
        ("README.md".to_string(), "readme", 0o644),
    ] {
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(mode);
        h.set_cksum();
        tar.append_data(&mut h, format!("{top}/{name}"), body.as_bytes())
            .unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap()
}

/// Serves `files` (path -> body) over HTTP on a local port until the test ends.
fn serve(files: Vec<(String, Vec<u8>)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let files = Arc::new(files);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let files = files.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap() == 0 || h == "\r\n" {
                        break;
                    }
                }
                match files.iter().find(|(p, _)| *p == path) {
                    Some((_, body)) => {
                        write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .unwrap();
                        stream.write_all(body).unwrap();
                    }
                    None => {
                        write!(stream, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                    }
                }
            });
        }
    });
    base
}

fn setup(sums_hash: Option<&str>) -> (PathBuf, PathBuf, anyos_update::Update) {
    let tag = "v9.9.9";
    let archive = release_archive(tag);
    let name = format!("anyos-{tag}-{TARGET}.tar.gz");
    let hash = sums_hash
        .map(str::to_string)
        .unwrap_or_else(|| format!("{:x}", Sha256::digest(&archive)));
    let sums = format!(
        "{hash}  {name}\n{}  anyos-{tag}-other.zip\n",
        "0".repeat(64)
    );
    let base = serve(vec![
        (format!("/dl/{name}"), archive),
        ("/dl/SHA256SUMS".into(), sums.into_bytes()),
    ]);
    let json = format!(
        r#"{{"tag_name":"{tag}","html_url":"{base}/release","assets":[
            {{"name":"{name}","browser_download_url":"{base}/dl/{name}"}},
            {{"name":"SHA256SUMS","browser_download_url":"{base}/dl/SHA256SUMS"}}]}}"#
    );
    let update = parse_latest(&json, &Version::parse("0.1.0").unwrap(), TARGET)
        .unwrap()
        .expect("9.9.9 is newer");

    let root = tmp(if sums_hash.is_some() { "bad" } else { "good" });
    let install = root.join("bin");
    fs::create_dir_all(&install).unwrap();
    fs::write(install.join(format!("anyos{EXE}")), "old cli").unwrap();
    fs::write(install.join(format!("anyos-gui{EXE}")), "old gui").unwrap();
    (root, install, update)
}

fn read(dir: &Path, name: &str) -> String {
    fs::read_to_string(dir.join(name)).unwrap()
}

#[test]
fn downloads_verifies_and_installs_a_release() {
    let (root, install, update) = setup(None);
    let layout = Layout::for_exe(&install.join(format!("anyos{EXE}"))).unwrap();
    let mut last = (0, None);
    let done = install_into(&update, &layout, &mut |d, t| last = (d, t)).unwrap();

    assert_eq!(done.version.to_string(), "9.9.9");
    assert_eq!(read(&install, &format!("anyos{EXE}")), "new cli");
    assert_eq!(read(&install, &format!("anyos-gui{EXE}")), "new gui");
    assert_eq!(last.1, Some(last.0), "progress reached the full size");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(install.join("anyos"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "installed binary is executable");
    }
    // Only the binaries and nothing else: no staging folder, no README.
    let mut left: Vec<_> = fs::read_dir(&install)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    left.sort();
    assert_eq!(left, [format!("anyos{EXE}"), format!("anyos-gui{EXE}")]);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn refuses_a_download_whose_checksum_does_not_match() {
    let (root, install, update) = setup(Some(&"ab".repeat(32)));
    let layout = Layout::for_exe(&install.join(format!("anyos{EXE}"))).unwrap();
    let err = install_into(&update, &layout, &mut |_, _| {}).unwrap_err();
    assert!(matches!(err, Error::ChecksumMismatch { .. }), "{err}");
    assert_eq!(read(&install, &format!("anyos{EXE}")), "old cli");
    assert_eq!(read(&install, &format!("anyos-gui{EXE}")), "old gui");
    assert_eq!(
        fs::read_dir(&install).unwrap().count(),
        2,
        "staging removed"
    );
    let _ = fs::remove_dir_all(&root);
}
