import hashlib
import json
from pathlib import Path
import stat
import tempfile
import unittest
import zipfile

from tools.smoke_release import ROOT, unpack


class ExtractedReleaseIntegrityTests(unittest.TestCase):
    def write_zip(self, directory, entries, hashes):
        path = directory / "candidate.zip"
        with zipfile.ZipFile(path, "w") as bundle:
            for name, data, mode in entries:
                entry = zipfile.ZipInfo(name)
                entry.external_attr = mode << 16
                bundle.writestr(entry, data)
            bundle.writestr("BUILD.json", json.dumps({"sha256": hashes}))
        path.with_suffix(".zip.sha256").write_text(hashlib.sha256(path.read_bytes()).hexdigest())
        return path

    def test_unsafe_entries_fail_before_extraction(self):
        base = ROOT / ".local/tmp"
        base.mkdir(parents=True, exist_ok=True)
        for name, mode in [("../escape", stat.S_IFREG), ("C:/escape", stat.S_IFREG), ("link", stat.S_IFLNK)]:
            with self.subTest(name=name), tempfile.TemporaryDirectory(dir=base) as temp:
                directory = Path(temp)
                output = directory / "output"
                output.mkdir()
                candidate = self.write_zip(directory, [(name, b"not executed", mode)], {name: hashlib.sha256(b"not executed").hexdigest()})
                with self.assertRaisesRegex(ValueError, "unsafe"):
                    unpack(candidate, output)
                self.assertEqual(list(output.iterdir()), [])
                self.assertFalse((directory / "escape").exists())

    def test_payload_change_cannot_pass_with_only_a_new_outer_checksum(self):
        with tempfile.TemporaryDirectory(dir=ROOT / ".local/tmp") as temp:
            directory = Path(temp)
            output = directory / "output"
            output.mkdir()
            candidate = self.write_zip(directory, [("server", b"modified", stat.S_IFREG)], {"server": hashlib.sha256(b"original").hexdigest()})
            with self.assertRaisesRegex(ValueError, "payload checksum"):
                unpack(candidate, output)
            self.assertFalse((output / "server").exists())


if __name__ == "__main__":
    unittest.main()
