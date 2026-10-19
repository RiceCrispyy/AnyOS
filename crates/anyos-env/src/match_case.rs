//! Host-path matching: `match_case` must honour the host's case and, on
//! Windows, the guest's case-insensitive `C:` drive.
//!
//! `match_case` reads each directory entry with `file_name()` first, so a
//! case-different name is found on case-insensitive hosts (including Windows).
//! The remaining risk is a stale in-memory directory that the host can see
//! but that `read_dir` skipped: `dir.join(name)` is tried before the scan, and
//! every `data_dir`, `envs/<name>` and per-key `create_key` directory is
//! created and never carried across environments, so a fresh directory is
//! always materialised before `match_case` runs. `host_path_in` accepts a
//! relative component, which is exactly how installers address files inside
//! the package's `Windows` directory, and forwards it to `host_path`-style
//! matching that enforces the `C:`-only, no-`..` and case rules.

use std::path::Path;
use std::path::PathBuf;

pub(crate) fn match_case(dir: &Path, name: &str) -> PathBuf {
    let exact = dir.join(name);
    if exact.exists() {
        return exact;
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_name()
                .to_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
            {
                return e.path();
            }
        }
    }
    exact
}
