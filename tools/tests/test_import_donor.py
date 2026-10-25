import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("import_donor", Path(__file__).resolve().parents[1] / "import_donor.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class AllowlistTests(unittest.TestCase):
    def test_approved_paths(self):
        for path in ["docs/rules/user.md", "mc/aspects/physics.md", "crates/voxel-core/src/world/mek/a.rs",
                     "crates/voxel-core/src/flight_tests.rs", "web/sounds/CREDITS.md", "web/ui/pixel.woff",
                     "web/scenes/forest.json", "web/atlas.png", "CREDITS.md"]:
            with self.subTest(path=path):
                self.assertTrue(module.allowed(path))

    def test_unapproved_paths_and_traversal(self):
        for path in ["../secret", "/web/atlas.png", "web/textures/../render/engine.js", "web/render/tests.rs",
                     "web/engine/bench_tests.rs", "web/shaders/sky.wgsl", "tasks/a.md", "reports/a.md",
                     "docs/HANDOFF.md", "docs/workflow.md", "Cargo.toml", "LICENSE", "web/ui/pixel.woff2",
                     "web/ui/other.js", "web/scenes/sub/forest.json", "web/textures/.git/config",
                     "web//atlas.png", "crates/voxel-core/src/lib.rs", "other/flight_tests.rs"]:
            with self.subTest(path=path):
                self.assertFalse(module.allowed(path))

    def test_verify_rejects_modified_missing_extra_and_symlink(self):
        import json
        old_dest = module.DEST
        (Path(__file__).resolve().parents[2] / ".local").mkdir(exist_ok=True)
        try:
            with tempfile.TemporaryDirectory(dir=Path(__file__).resolve().parents[2] / ".local") as directory:
                module.DEST = Path(directory)
                path = module.DEST / "CREDITS.md"
                path.write_text("reference data")
                manifest = {"commit": module.COMMIT, "remote": module.REMOTE,
                            "files": [{"path": "CREDITS.md", "bytes": path.stat().st_size,
                                       "sha256": module.sha256(path), "git_blob": module.git_blob_id(path.read_bytes())}]}
                (module.DEST / module.MANIFEST).write_text(json.dumps(manifest))
                module.verify()
                path.write_text("modified")
                with self.assertRaises(RuntimeError): module.verify()
                path.write_text("reference data")
                extra = module.DEST / "unexpected"
                extra.write_text("no")
                with self.assertRaises(RuntimeError): module.verify()
                extra.unlink()
                path.unlink()
                with self.assertRaises(RuntimeError): module.verify()
                path.symlink_to(module.DEST / module.MANIFEST)
                with self.assertRaises(RuntimeError): module.verify()
        finally:
            module.DEST = old_dest


if __name__ == "__main__":
    unittest.main()
