#!/usr/bin/env python3
"""Verified repo-local Debian ALSA development headers; no system changes."""
import hashlib
import lzma
from pathlib import Path
import re
import subprocess

root = Path(__file__).resolve().parents[1]
cache = root / ".local/audio-download"
cache.mkdir(parents=True, exist_ok=True)
if "VERSION_CODENAME=trixie" not in Path("/etc/os-release").read_text():
    raise SystemExit("Use the host's trusted libasound2-dev package outside Debian13.")
if subprocess.check_output(["dpkg", "--print-architecture"], text=True).strip() != "amd64":
    raise SystemExit("Repo-local helper currently requires Debian13 amd64.")


def fetch(url, path):
    subprocess.run(["curl", "--fail", "--silent", "--show-error", "--location",
                    "--proto", "=https", "--tlsv1.2", url, "-o", str(path)], check=True)


fetch("https://deb.debian.org/debian/dists/trixie/InRelease", cache / "InRelease")
release = cache / "Release"
release.unlink(missing_ok=True)
subprocess.run(["sqv", "--keyring", "/usr/share/keyrings/debian-archive-keyring.pgp",
                "--output", str(release), "--cleartext", str(cache / "InRelease")], check=True)
text = release.read_text()
if "\nCodename: trixie\n" not in text:
    raise SystemExit("Unexpected signed Debian release.")
index_name = "main/binary-amd64/Packages.xz"
metadata = next((line.split() for line in text.split("\nSHA256:\n", 1)[1].splitlines()
                 if len(line.split()) == 3 and line.split()[2] == index_name), None)
if metadata is None:
    raise SystemExit("Authenticated index metadata missing.")


def verified(url, path, expected_hash, expected_size):
    if not re.fullmatch("[0-9a-f]{64}", expected_hash) or not 0 < expected_size < 100_000_000:
        raise SystemExit("Invalid signed artifact metadata.")
    fetch(url, path)
    data = path.read_bytes()
    if len(data) != expected_size or hashlib.sha256(data).hexdigest() != expected_hash:
        raise SystemExit("Artifact SHA256/size verification failed.")


index = cache / "Packages.xz"
verified("https://deb.debian.org/debian/dists/trixie/" + index_name, index,
         metadata[0], int(metadata[1]))
package = None
for block in lzma.decompress(index.read_bytes()).decode().split("\n\n"):
    if block.startswith("Package: libasound2-dev\n"):
        package = dict(line.split(": ", 1) for line in block.splitlines()
                       if ": " in line and not line[0].isspace())
        break
if package is None or package.get("Architecture") != "amd64":
    raise SystemExit("Expected authenticated ALSA development package missing.")
filename = package["Filename"]
if not re.fullmatch(r"pool/main/a/alsa-lib/libasound2-dev_[a-zA-Z0-9.+~:-]+_amd64\.deb", filename):
    raise SystemExit("Unexpected ALSA package path.")
archive = cache / "libasound2-dev.deb"
verified("https://deb.debian.org/debian/" + filename, archive, package["SHA256"], int(package["Size"]))
prefix = root / ".local/audio-deps"
subprocess.run(["dpkg-deb", "--extract", str(archive), str(prefix)], check=True)
pc = prefix / "usr/lib/x86_64-linux-gnu/pkgconfig/alsa.pc"
pc_text = pc.read_text()
if "prefix=/usr\n" not in pc_text:
    raise SystemExit("Unexpected ALSA pkg-config layout.")
pc.write_text(pc_text.replace("prefix=/usr\n", "prefix=${pcfiledir}/../../..\n", 1))
runtime = Path("/lib/x86_64-linux-gnu/libasound.so.2")
if not runtime.is_file():
    raise SystemExit("Host ALSA runtime is missing; install it through trusted host packages.")
runtime_link = prefix / "usr/lib/x86_64-linux-gnu/libasound.so.2"
if not runtime_link.exists():
    runtime_link.symlink_to(runtime)
development_link = prefix / "usr/lib/x86_64-linux-gnu/libasound.so"
development_link.unlink(missing_ok=True)
development_link.symlink_to(runtime)
print("AUDIO_HEADERS_READY version=" + package["Version"] + " pkgconfig=" + str(pc.parent))
