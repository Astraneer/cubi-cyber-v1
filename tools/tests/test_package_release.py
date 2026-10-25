import hashlib
import json
from pathlib import Path
import tempfile
from unittest import TestCase
import zipfile

from tools.package_release import ROOT, archive, local, payload


class PackageTests(TestCase):
    def test_explicit_payload_omits_identity_saves_and_unlisted_files(self):
        base = ROOT / ".local/tmp"
        base.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=base) as temp:
            directory = Path(temp)
            for name in ("client-native", "server", "examples/benchmark", "identity", "saves/world"):
                path = directory / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b"\x7fELF-test")
            files = payload("native", directory, "linux")
            self.assertNotIn("identity", files)
            self.assertFalse(any(name.startswith("saves/") for name in files))
            with self.assertRaises(ValueError):
                payload("native", directory, "windows")

    def test_archive_is_reproducible_and_every_payload_hash_verifies(self):
        with tempfile.TemporaryDirectory(dir=ROOT / ".local/tmp") as temp:
            first, second = Path(temp) / "a.zip", Path(temp) / "b.zip"
            files = {"client-native": b"sample executable fixture", "docs/STATUS.md": "Статус".encode()}
            archive(first, files, {"sourceCommit": "fixture"})
            archive(second, files, {"sourceCommit": "fixture"})
            self.assertEqual(first.read_bytes(), second.read_bytes())
            with zipfile.ZipFile(first) as bundle:
                manifest = json.loads(bundle.read("BUILD.json"))
                for name, digest in manifest["sha256"].items():
                    self.assertEqual(hashlib.sha256(bundle.read(name)).hexdigest(), digest)
                self.assertEqual(bundle.getinfo("client-native").external_attr >> 16 & 0o777, 0o755)
            digest = hashlib.sha256(first.read_bytes()).hexdigest()
            self.assertEqual(first.with_suffix(".zip.sha256").read_text(), f"{digest}  a.zip\n")

    def test_outside_paths_and_zip_traversal_fail_before_write(self):
        with self.assertRaises(ValueError):
            local(ROOT.parent / "outside.zip")
        for name in ("../identity", "/absolute", "path\\file"):
            with self.subTest(name=name), self.assertRaises(ValueError):
                archive(ROOT / ".local/tmp/forbidden.zip", {name: b"fixture"}, {})
