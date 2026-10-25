#!/usr/bin/env bash
# Debian 13 / amd64 cloud fallback. No root, no system writes, no disabled checks.
set -euo pipefail
cubi_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$cubi_root"
if [[ "$(dpkg --print-architecture)" != amd64 ]] || ! rg -q '^VERSION_CODENAME=trixie$' /etc/os-release; then
  echo 'Repo-local lavapipe helper requires Debian 13 (trixie), amd64; use trusted OS packages on other hosts.' >&2
  exit 1
fi
command -v sqv >/dev/null
cubi_cache="$cubi_root/.local/mesa-download"
mkdir -p "$cubi_cache"
curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
  https://deb.debian.org/debian/dists/trixie/InRelease -o "$cubi_cache/InRelease"
rm -f -- "$cubi_cache/Release"
# sqv is Debian 13 APT's OpenPGP verifier; its policy and trust checks stay enabled.
sqv --keyring /usr/share/keyrings/debian-archive-keyring.pgp \
  --output "$cubi_cache/Release" --cleartext "$cubi_cache/InRelease"
python3 - <<'PY'
import hashlib
import json
import lzma
import re
import subprocess
from pathlib import Path, PurePosixPath

root = Path.cwd()
cache = root / '.local/mesa-download'
release = (cache / 'Release').read_text()
if '\nCodename: trixie\n' not in release or '\nArchitectures:' not in release:
    raise SystemExit('Unexpected signed Debian release')


def download(url, path, expected, size):
    if not re.fullmatch('[0-9a-f]{64}', expected) or not 0 < size <= 100_000_000:
        raise SystemExit('Invalid signed artifact metadata')
    subprocess.run([
        'curl', '--fail', '--silent', '--show-error', '--location',
        '--proto', '=https', '--tlsv1.2', url, '-o', str(path),
    ], check=True)
    data = path.read_bytes()
    if len(data) != size or hashlib.sha256(data).hexdigest() != expected:
        raise SystemExit(f'SHA256/size verification failed: {path.name}')


checksums = release.split('\nSHA256:\n', 1)[1]
index_name = 'main/binary-amd64/Packages.xz'
for line in checksums.splitlines():
    fields = line.split()
    if len(fields) == 3 and fields[2] == index_name:
        index_hash, index_size, _ = fields
        break
else:
    raise SystemExit('Signed package-index hash missing')
index = cache / 'Packages.xz'
download(
    'https://deb.debian.org/debian/dists/trixie/' + index_name,
    index, index_hash, int(index_size),
)
text = lzma.decompress(index.read_bytes()).decode()
for block in text.split('\n\n'):
    if block.startswith('Package: mesa-vulkan-drivers\n'):
        package = dict(
            line.split(': ', 1) for line in block.splitlines()
            if ': ' in line and not line[0].isspace()
        )
        break
else:
    raise SystemExit('Mesa package missing in authenticated index')
filename = package['Filename']
if (
    package['Architecture'] != 'amd64'
    or not filename.startswith('pool/main/m/mesa/mesa-vulkan-drivers_')
    or not filename.endswith('.deb')
    or '..' in PurePosixPath(filename).parts
):
    raise SystemExit('Unexpected Mesa package path or architecture')
archive = cache / 'mesa-vulkan-drivers.deb'
download(
    'https://deb.debian.org/debian/' + filename,
    archive, package['SHA256'], int(package['Size']),
)
prefix = root / '.local/mesa'
subprocess.run(['dpkg-deb', '--extract', str(archive), str(prefix)], check=True)
library = prefix / 'usr/lib/x86_64-linux-gnu/libvulkan_lvp.so'
original_icd = prefix / 'usr/share/vulkan/icd.d/lvp_icd.json'
if not library.is_file() or not original_icd.is_file():
    raise SystemExit('Authenticated Mesa package has no expected lavapipe files')
dependencies = subprocess.run(['ldd', str(library)], text=True, capture_output=True, check=True)
if 'not found' in dependencies.stdout:
    raise SystemExit('Install required OS runtime libraries: ' + dependencies.stdout)
config = json.loads(original_icd.read_text())
config['ICD']['library_path'] = str(library.resolve())
icd = prefix / 'lvp-local.json'
icd.write_text(json.dumps(config, indent=2) + '\n')
(cache / 'verified-package.json').write_text(json.dumps({
    'version': package['Version'], 'filename': filename,
    'sha256': package['SHA256'], 'index_sha256': index_hash,
}, indent=2) + '\n')
print(f"LAVAPIPE_READY version={package['Version']} ICD={icd}")
PY
