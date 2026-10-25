#!/usr/bin/env python3
"""Local WASM development server; no deployment, credentials or remote access."""
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import unquote, urlsplit
import argparse


class WebHandler(SimpleHTTPRequestHandler):
    extensions_map = {**SimpleHTTPRequestHandler.extensions_map, ".wasm": "application/wasm"}

    def end_headers(self):
        self.send_header("Cross-Origin-Opener-Policy", "same-origin")
        self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
        self.send_header("Cross-Origin-Resource-Policy", "same-origin")
        self.send_header("Cache-Control", "no-store")
        super().end_headers()

    def send_head(self):
        # SimpleHTTPRequestHandler follows symlinks; do not serve files outside
        # the selected distribution even when a local symlink points there.
        base = Path(self.directory).resolve()
        path = base / unquote(urlsplit(self.path).path).lstrip("/")
        if not path.resolve().is_relative_to(base):
            self.send_error(403, "Outside development root")
            return None
        return super().send_head()


def server(directory, port=8000):
    return ThreadingHTTPServer(("127.0.0.1", port), partial(WebHandler, directory=str(Path(directory).resolve())))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=Path, default=Path(__file__).resolve().parents[1] / ".local/web-dist")
    parser.add_argument("--port", type=int, default=8000)
    args = parser.parse_args()
    if not (args.directory / "index.html").is_file():
        parser.error("Web build missing; run bash tools/build_web.sh first")
    with server(args.directory, args.port) as httpd:
        print(f"WEB_SERVER_PORT={httpd.server_port}", flush=True)
        try:
            httpd.serve_forever()
        except KeyboardInterrupt:
            pass


if __name__ == "__main__":
    main()
