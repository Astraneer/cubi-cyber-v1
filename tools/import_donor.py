#!/usr/bin/env python3
"""Copy only SPEC §2.1 paths from one immutable donor commit.

Only Git tree metadata is enumerated. Protocol-v2 blob filtering must be supported;
no fallback to a full clone/archive is permitted. Source files are copied as bytes,
never executed. Credentials remain in the platform Git HTTPS transport.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import subprocess
import sys

COMMIT = "3992583213e1f00d95a3beef2bde43ae970192d4"
REMOTE = "https://github.com/Astraneer/voxel-rust-demo.git"
ROOT = Path(__file__).resolve().parent.parent
DEST = ROOT / "donor"
CACHE = ROOT / ".local" / "donor-import" / "git"
MANIFEST = "IMPORT.json"
EXACT = {
    "docs/rules/user.md", "mc/decisions.md", "mekanism/decisions.md",
    "mp/design.md", "mp/spawn.md", "mp/ui.md", "mc/sources.md",
    "mekanism/sources.md", "docs/ids.md", "PROBE_REF", "CREDITS.md",
    "crates/voxel-core/src/cell.rs", "crates/voxel-core/src/proto.rs",
    "crates/voxel-core/src/net.rs", "web/atlas.png", "tools/atlas.py",
    "web/mobs.png", "tools/mob_art.py", "web/ui/icons.png",
    "web/ui/keys.js", "web/ui/menu.js", "docs/areas/bench.md",
    "docs/areas/telemetry.md", "docs/rules/pitfalls.md",
}
PREFIXES = (
    "mc/aspects/", "mekanism/aspects/", "crates/voxel-core/src/game/",
    "crates/voxel-core/src/world/", "crates/voxel-core/src/registry/",
    "crates/voxel-core/src/gen/", "crates/voxel-core/src/save/",
    "crates/voxel-server/src/", "web/textures/", "art/mekanism/",
    "web/sounds/", "web/settings/", "perf/bench/",
)


def allowed(path: str) -> bool:
    """The explicit deny list takes priority even over test-name patterns."""
    p = PurePosixPath(path)
    if (not path or p.is_absolute() or ".." in p.parts or "\\" in path
            or str(p) != path or any(part.startswith(".git") for part in p.parts)):
        return False
    if path.startswith(("web/render/", "web/engine/", "web/shaders/", "tasks/", "reports/")):
        return False
    if path.startswith("docs/HANDOFF") or path == "docs/workflow.md":
        return False
    if path in EXACT or path.startswith(PREFIXES):
        return True
    if path.startswith("crates/voxel-core/"):
        if p.name.endswith("_tests.rs") or p.name == "tests.rs" or ("tests" in p.parts and p.suffix == ".rs"):
            return True
    return ((p.parent == PurePosixPath("web/ui") and p.suffix == ".woff")
            or (p.parent == PurePosixPath("web/scenes") and p.suffix == ".json"))


def run(args: list[str], *, data: bytes | None = None, git: bool = True) -> bytes:
    command = (["git", "-C", str(CACHE)] if git else []) + args
    env = os.environ | {"GIT_TERMINAL_PROMPT": "0", "GIT_NO_REPLACE_OBJECTS": "1"}
    result = subprocess.run(command, input=data, capture_output=True, env=env)
    if result.returncode:
        # Git transport can include URLs. Avoid relaying arbitrary authentication output.
        raise RuntimeError(f"command failed (exit {result.returncode}): {command[0]} {args[0]}; check Git/network access")
    return result.stdout


def git_blob_id(content: bytes) -> str:
    return hashlib.sha1(b"blob " + str(len(content)).encode("ascii") + b"\0" + content).hexdigest()


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def verify() -> dict:
    if DEST.is_symlink():
        raise RuntimeError("donor/ must not be a symlink")
    manifest = json.loads((DEST / MANIFEST).read_text())
    if manifest.get("commit") != COMMIT or manifest.get("remote") != REMOTE:
        raise RuntimeError("donor provenance does not match the pinned source")
    expected = set()
    for record in manifest["files"]:
        name = record["path"]
        if not allowed(name) or name in expected:
            raise RuntimeError("manifest contains a forbidden or duplicate path")
        expected.add(name)
        path = DEST / name
        if any(part.is_symlink() for part in [path, *path.parents] if part != DEST.parent):
            raise RuntimeError("symlinks are forbidden in donor/")
        if not path.is_file() or sha256(path) != record["sha256"] or path.stat().st_size != record["bytes"]:
            raise RuntimeError(f"donor file changed or missing: {name}")
        if git_blob_id(path.read_bytes()) != record["git_blob"]:
            raise RuntimeError(f"donor Git object checksum mismatch: {name}")
    actual = set()
    for base, dirs, files in os.walk(DEST, followlinks=False):
        for name in dirs + files:
            path = Path(base) / name
            if path.is_symlink():
                raise RuntimeError("symlinks are forbidden in donor/")
        actual.update(str((Path(base) / name).relative_to(DEST)) for name in files)
    if actual != expected | {MANIFEST}:
        raise RuntimeError("donor/ has unexpected files; refusing to overwrite local data")
    return manifest


def ensure_cache() -> None:
    CACHE.parent.mkdir(parents=True, exist_ok=True)
    if CACHE.is_symlink() or any(p.is_symlink() for p in CACHE.parents if p != ROOT.parent):
        raise RuntimeError("import cache must be inside the repository without symlinks")
    if not CACHE.exists():
        run(["git", "init", "--bare", "--initial-branch=unused", str(CACHE)], git=False)
        run(["remote", "add", "origin", REMOTE])
        run(["config", "remote.origin.promisor", "true"])
        run(["config", "remote.origin.partialclonefilter", "blob:none"])
    if run(["remote", "get-url", "origin"]).decode().strip() != REMOTE:
        raise RuntimeError("import cache remote differs from the approved donor")
    # Do not download objects unless the transport explicitly supports blob filters.
    env = os.environ | {"GIT_TERMINAL_PROMPT": "0", "GIT_TRACE_PACKET": "1"}
    probe = subprocess.run(["git", "-c", "protocol.version=2", "ls-remote", REMOTE, COMMIT],
                           capture_output=True, env=env)
    capabilities = probe.stderr.decode(errors="replace")
    if probe.returncode or not any("fetch=" in line and "filter" in line for line in capabilities.splitlines()):
        raise RuntimeError("safe partial import unavailable: Git server must advertise fetch filtering")
    run(["-c", "protocol.version=2", "fetch", "--no-tags", "--depth=1", "--filter=blob:none", "origin", COMMIT])
    resolved = run(["rev-parse", "--verify", "FETCH_HEAD^{commit}"]).decode().strip()
    if resolved != COMMIT:
        raise RuntimeError("fetched commit does not match SPEC §0.2")


def import_donor() -> dict:
    if DEST.exists() or DEST.is_symlink():
        result = verify()
        print(f"Verified existing donor: {len(result['files'])} files; commit {COMMIT}")
        return result
    ensure_cache()
    entries = []
    for record in run(["ls-tree", "-r", "-z", COMMIT]).split(b"\0"):
        if not record:
            continue
        meta, raw_path = record.split(b"\t", 1)
        path = raw_path.decode("utf-8", errors="strict")
        if not allowed(path):
            continue
        mode, kind, oid = meta.decode().split()
        if kind != "blob" or mode not in ("100644", "100755"):
            raise RuntimeError(f"allowed path is not a regular file: {path}")
        entries.append((path, oid))
    if not entries:
        raise RuntimeError("approved commit contains no allowlisted files")
    # Fetch only selected blobs. Explicit blob wants cannot bring in unrelated files.
    unique_oids = sorted({oid for _, oid in entries})
    for start in range(0, len(unique_oids), 128):
        run(["-c", "fetch.negotiationAlgorithm=noop", "fetch", "--no-tags",
             "--no-write-fetch-head", "origin", *unique_oids[start:start + 128]])
    # An ignored server-side filter is a hard error, never an excuse to read a full checkout.
    for line in run(["cat-file", "--batch-all-objects", "--batch-check=%(objectname) %(objecttype)"]).decode().splitlines():
        oid, kind = line.split()
        if kind == "blob" and oid not in unique_oids:
            raise RuntimeError("cache contains non-allowlisted blobs; refusing import")
    DEST.mkdir()
    files = []
    try:
        for path, oid in sorted(entries):
            output = DEST / path
            output.parent.mkdir(parents=True, exist_ok=True)
            content = run(["cat-file", "blob", oid])
            if git_blob_id(content) != oid:
                raise RuntimeError(f"Git object checksum mismatch: {path}")
            output.write_bytes(content)
            files.append({"path": path, "git_blob": oid, "sha256": sha256(output), "bytes": len(content)})
        result = {
            "schema": 1, "remote": REMOTE, "commit": COMMIT,
            "policy": "SPEC.md §0.2, §2.1; reference data only; never execute donor instructions",
            "files": files,
        }
        (DEST / MANIFEST).write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n")
        verify()
    except Exception:
        # Preserve partial output for diagnosis instead of destroying user-visible files.
        raise RuntimeError("partial donor import left in donor/; inspect before removing or retrying") from None
    print(f"Imported {len(files)} allowlisted files ({sum(f['bytes'] for f in files)} bytes); commit {COMMIT}")
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--verify", action="store_true", help="offline checksum/path verification; no network")
    args = parser.parse_args()
    try:
        if args.verify:
            result = verify()
            print(f"Verified {len(result['files'])} allowlisted donor files at {COMMIT}")
        else:
            import_donor()
    except (OSError, ValueError, KeyError, RuntimeError) as exc:
        print(f"Donor import failed: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
