import os
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest import TestCase, mock
from tools.check_diagnostic import diagnostic, read_diagnostic


class DiagnosticTests(TestCase):
    def test_backend_message_keeps_panic_context_and_escapes_annotation_data(self):
        text = "noise\nthread 'main' panicked at render.rs:9:\nValidation Error\nSurface 50%\n\x1b[31mCaused by:\x1b[0m\nUnknown adapter\n"
        value = diagnostic(text)
        self.assertIn("render.rs:9", value)
        self.assertIn("Surface 50%25", value)
        self.assertIn("Unknown adapter", value)
        self.assertNotIn("\x1b", value)
        self.assertNotIn("\n", value)

    def test_output_is_bounded_and_known_tokens_are_redacted(self):
        with mock.patch.dict(os.environ, {"GITHUB_TOKEN": "a-private-test-value"}):
            value = diagnostic("error: " + "x" * 3000 + " a-private-test-value")
        self.assertLessEqual(len(value), 1800)
        self.assertNotIn("a-private-test-value", value)
        self.assertIn("[redacted]", value)

    def test_pixel_arrays_keep_the_actual_panic_location(self):
        with TemporaryDirectory() as folder:
            path = Path(folder) / "render.log"
            path.write_text("thread 'coverage' panicked at tests/horizon_gpu.rs:155:\n"
                            "assertion `left == right` failed\n"
                            "left: [" + "255, " * 200000 + "]\n"
                            "right: [" + "0, " * 200000 + "]\n"
                            "error: test failed, to rerun pass `-p client --test horizon_gpu`\n")
            value = read_diagnostic(path)
            self.assertIn("horizon_gpu.rs:155", value)
            self.assertIn("left: [255", value)
            self.assertIn("right: [0", value)
            self.assertLessEqual(len(value), 1800)
