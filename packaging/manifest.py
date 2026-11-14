"""Writes latest.json and SHA256SUMS.txt for a release.

    python3 packaging/manifest.py 0.1.0 owner/repo dist

latest.json is what Wercode reads to update itself (crates/core/src/update.rs):
the version and, for each platform, the file to download with its SHA-256.
"""

import hashlib
import json
import pathlib
import sys

version, repo, folder = sys.argv[1], sys.argv[2], pathlib.Path(sys.argv[3])
files = {
    "windows-x86_64": f"Wercode-Setup-{version}-x64.exe",
    "macos": f"Wercode-{version}-macos.tar.gz",
    "linux-x86_64": f"Wercode-{version}-linux-x64.tar.gz",
}
base = f"https://github.com/{repo}/releases/download/v{version}"


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


assets = {}
for platform, name in files.items():
    path = folder / name
    if not path.exists():
        sys.exit(f"missing {path}")
    assets[platform] = {"url": f"{base}/{name}", "sha256": sha256(path)}

manifest = {
    "version": version,
    "notes": f"https://github.com/{repo}/releases/tag/v{version}",
    "assets": assets,
}
(folder / "latest.json").write_text(json.dumps(manifest, indent=2) + "\n")
sums = "".join(f"{sha256(p)}  {p.name}\n" for p in sorted(folder.iterdir()) if p.is_file() and p.name != "SHA256SUMS.txt")
(folder / "SHA256SUMS.txt").write_text(sums)
