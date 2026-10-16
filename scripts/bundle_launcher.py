#!/usr/bin/env python3
"""bundle_launcher.py — assemble the Xcode-free AnyOS .app.

Copies the macOS build of anyos into the bundle, fills the bundled binaries
directory from tests/fixtures/bin/other, and stamps the Info.plist version.

Run from the repository root. Requires only the Python 3 standard library.
"""

import shutil
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LAUNCHER = ROOT / "anyos" / "Launcher"
BUNDLE = LAUNCHER / "Contents" / "MacOS"
RESOURCE_BIN = LAUNCHER / "Contents" / "Resources" / "binary"
RESOURCE_DOC = LAUNCHER / "Contents" / "Resources" / "doc"
TARGET_ANYOS = ROOT / "target" / "release" / "anyos"


def fail(msg: str) -> None:
    print(f"bundle_launcher.py: {msg}", file=sys.stderr)
    sys.exit(1)


def main() -> None:
    # 1. The macOS anyos binary.
    if not TARGET_ANYOS.exists():
        fail(f"missing {TARGET_ANYOS} — build with `cargo build --release` on "
             f"macOS first")
    BUNDLE.mkdir(parents=True, exist_ok=True)
    shutil.copy2(TARGET_ANYOS, BUNDLE / "anyos")
    BUNDLE.chmod(0o755)

    # 2. Bundled guest programs.
    source_dir = ROOT / "tests" / "fixtures" / "bin" / "other"
    if source_dir.exists():
        RESOURCE_BIN.mkdir(parents=True, exist_ok=True)
        for child in sorted(source_dir.iterdir()):
            if child.is_file():
                shutil.copy2(child, RESOURCE_BIN / child.name)

    # 3. Help and LICENSE inside the app.
    for src in (ROOT / "LICENSE.md", ROOT / "README.md"):
        dst = RESOURCE_DOC / src.name
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(src, dst)

    # 4. Give the app a self-contained name.
    app = LAUNCHER / "AnyOSLauncher.app"
    if app.exists():
        shutil.rmtree(app)
    shutil.move(str(LAUNCHER), str(app))

    print(f"Bundled {BUNDLE / 'anyos'} -> {app}")
    print(f"Bundled binaries:{' '.join(p.name for p in RESOURCE_BIN.iterdir())}")
    print(f"Docs: {RESOURCE_DOC}")


if __name__ == "__main__":
    main()
