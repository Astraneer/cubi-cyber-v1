import http.client
from pathlib import Path
import tempfile
import threading
import unittest

from serve_web import server


class WebServerTests(unittest.TestCase):
    def setUp(self):
        # Keep generated state within the repository, including in CI.
        cache = Path(__file__).resolve().parents[1] / ".local/tmp"
        cache.mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=cache)
        self.base = Path(self.temp.name)
        (self.base / "index.html").write_text("<canvas></canvas>")
        (self.base / "module.wasm").write_bytes(b"\x00asm\x01\x00\x00\x00")
        self.httpd = server(self.base, 0)
        self.thread = threading.Thread(target=self.httpd.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self):
        self.httpd.shutdown()
        self.httpd.server_close()
        self.thread.join()
        self.temp.cleanup()

    def request(self, path):
        conn = http.client.HTTPConnection("127.0.0.1", self.httpd.server_port, timeout=5)
        conn.request("GET", path)
        response = conn.getresponse()
        body = response.read()
        headers = dict(response.getheaders())
        conn.close()
        return response.status, headers, body

    def test_wasm_mime_and_isolation_headers(self):
        status, headers, body = self.request("/module.wasm")
        self.assertEqual(status, 200)
        self.assertEqual(headers["Content-type"], "application/wasm")
        self.assertEqual(headers["Cross-Origin-Opener-Policy"], "same-origin")
        self.assertEqual(headers["Cross-Origin-Embedder-Policy"], "require-corp")
        self.assertEqual(body[:4], b"\x00asm")

    def test_missing_resource_is_failure(self):
        status, _, _ = self.request("/missing.wasm")
        self.assertEqual(status, 404)

    def test_path_traversal_is_rejected(self):
        status, _, _ = self.request("/%2e%2e/SPEC.md")
        self.assertEqual(status, 403)

    def test_external_symlink_is_rejected(self):
        try:
            (self.base / "outside").symlink_to(self.base.parent / "outside.txt")
        except (OSError, NotImplementedError):
            self.skipTest("Host cannot create symlinks")
        status, _, _ = self.request("/outside")
        self.assertEqual(status, 403)


if __name__ == "__main__":
    unittest.main()
