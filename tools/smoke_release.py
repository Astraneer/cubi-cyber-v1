#!/usr/bin/env python3
"""Проверить хеши ZIP и запустить именно распакованные программы/сайт."""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import queue
import re
import socket
import stat
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parents[1]


def unpack(archive, destination):
    checksum = archive.with_suffix(archive.suffix + ".sha256").read_text(encoding="utf-8").split()[0]
    if hashlib.sha256(archive.read_bytes()).hexdigest() != checksum:
        raise ValueError("ZIP checksum mismatch")
    with zipfile.ZipFile(archive) as bundle:
        entries = bundle.infolist()
        names = [entry.filename for entry in entries]
        if len(names) != len(set(names)) or sum(e.file_size for e in entries) > 1 << 30:
            raise ValueError("duplicate entries or excessive extracted size")
        for entry in entries:
            name = PurePosixPath(entry.filename)
            mode = entry.external_attr >> 16
            if (name.is_absolute() or ".." in name.parts or "\\" in entry.filename
                    or ":" in entry.filename or not name.parts
                    or stat.S_IFMT(mode) not in (0, stat.S_IFREG)):
                raise ValueError("unsafe release entry")
        manifest = json.loads(bundle.read("BUILD.json"))
        if set(names) != set(manifest["sha256"]) | {"BUILD.json"}:
            raise ValueError("unlisted release payload")
        for entry in entries:
            data = bundle.read(entry)
            if entry.filename != "BUILD.json" and hashlib.sha256(data).hexdigest() != manifest["sha256"][entry.filename]:
                raise ValueError("payload checksum mismatch")
            path = destination.joinpath(*PurePosixPath(entry.filename).parts)
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
            path.chmod((entry.external_attr >> 16) & 0o777)
    return manifest


def stop(process):
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)


def exact(sock, length):
    value = b""
    while len(value) < length:
        part = sock.recv(length - len(value))
        if not part:
            raise ValueError("closed release WebSocket")
        value += part
    return value


def probe_websocket(address):
    host, port = address.removeprefix("ws://").rstrip("/").rsplit(":", 1)
    key = base64.b64encode(os.urandom(16)).decode()
    with socket.create_connection((host, int(port)), timeout=5) as sock:
        sock.sendall((f"GET / HTTP/1.1\r\nHost: {host}:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n").encode())
        response = b""
        while not response.endswith(b"\r\n\r\n"):
            response += exact(sock, 1)
            if len(response) > 8192:
                raise ValueError("excessive release handshake")
        expected = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest())
        if not response.startswith(b"HTTP/1.1 101") or expected.lower() not in response.lower():
            raise ValueError("release WebSocket upgrade failed")
        payload = b"cubi-release-smoke"
        mask = os.urandom(4)
        sock.sendall(bytes((0x89, 0x80 | len(payload))) + mask + bytes(value ^ mask[i % 4] for i, value in enumerate(payload)))
        header = exact(sock, 2)
        if header != bytes((0x8A, len(payload))) or exact(sock, len(payload)) != payload:
            raise ValueError("release WebSocket pong failed")


def native(directory, manifest, evidence):
    extension = ".exe" if manifest["platform"] == "windows" else ""
    environment = dict(os.environ)
    if extension:
        windows = Path(os.environ.get("SystemRoot", "C:/Windows"))
        environment["PATH"] = os.pathsep.join(map(str, [directory, windows / "System32", windows]))
    with (evidence / "server.log").open("w", encoding="utf-8") as log:
        ready = directory / "ready"
        process = subprocess.Popen([str(directory / ("server" + extension)), "--data", str(directory / "smoke-world"), "--no-generation", "--ws", "127.0.0.1:0", "--udp", "127.0.0.1:0", "--ready-file", str(ready)], cwd=directory, env=environment, stdout=log, stderr=subprocess.STDOUT)
        try:
            deadline = time.monotonic() + 10
            while not ready.exists():
                if process.poll() is not None or time.monotonic() >= deadline:
                    raise ValueError("extracted server did not become ready")
                time.sleep(0.05)
            probe_websocket(re.search(r"ws=(\S+)", ready.read_text(encoding="utf-8"))[1])
        finally:
            stop(process)
    binary = str(directory / ("client-native" + extension))
    run = subprocess.run([binary, "--software", "--scene", "sky", "--smoke-frames", "3"], cwd=directory, env=environment, capture_output=True, text=True, encoding="utf-8", timeout=120)
    (evidence / "native.log").write_text(run.stdout + run.stderr, encoding="utf-8")
    if run.returncode or "NATIVE_SKY_OK frames=3" not in run.stdout:
        raise ValueError("extracted native sky did not present three frames")
    route = evidence / "window.json"
    run = subprocess.run([binary, "--software", "--benchmark-frames", "18", "--benchmark", str(route)], cwd=directory, env=environment, capture_output=True, text=True, encoding="utf-8", timeout=180)
    (evidence / "window.log").write_text(run.stdout + run.stderr, encoding="utf-8")
    if run.returncode:
        raise ValueError("extracted window route failed")
    report = json.loads(route.read_text(encoding="utf-8"))
    benchmark = report["benchmark"]
    if (report["captureMode"] != "route" or not benchmark["presentationIncluded"]
            or benchmark["routeComplete"] or benchmark["framesRequested"] != 18
            or benchmark["routeFrames"] != 18 or benchmark["renderedFrames"] != 18
            or len(report["frames"]) != 18
            or not any(frame["gpuTotalComplete"] for frame in report["frames"])):
        raise ValueError("extracted route has incomplete physical frame evidence")
    subprocess.run([sys.executable, str(directory / "bench_report.py"), "report", str(route), "--out", str(evidence / "window.html")], cwd=directory, check=True)
    return {"webSocketPing": True, "nativePresentedFrames": 3, "windowRouteFrames": 18, "full1800Route": False}


def web(directory, evidence, browser):
    process = subprocess.Popen([sys.executable, str(directory / "serve_web.py"), "--directory", str(directory), "--port", "0"], cwd=directory, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, encoding="utf-8")
    lines = queue.Queue()
    threading.Thread(target=lambda: lines.put(process.stdout.readline()), daemon=True).start()
    try:
        line = lines.get(timeout=10)
        port = re.search(r"WEB_SERVER_PORT=(\d+)", line)
        if not port:
            raise ValueError("extracted web helper did not start")
        for name in ["index.html", "pkg/client_web.js", "pkg/client_web_bg.wasm"]:
            actual = urllib.request.urlopen(f"http://127.0.0.1:{port[1]}/{name}", timeout=10).read()
            if actual != (directory / name).read_bytes():
                raise ValueError("extracted site served different payload")
    finally:
        stop(process)
    if browser:
        environment = {**os.environ, "CUBI_WEB_DIST": str(directory), "CUBI_WEB_SERVER_SCRIPT": str(directory / "serve_web.py")}
        command = ["node", str(ROOT / "tools/web_smoke.mjs")]
        if os.environ.get("DISPLAY"):
            command.append("--headed")
        run = subprocess.run(command, cwd=ROOT, env=environment, capture_output=True, text=True, encoding="utf-8", timeout=120)
        (evidence / "browser.log").write_text(run.stdout + run.stderr, encoding="utf-8")
        if run.returncode:
            raise ValueError("extracted WebGPU sky failed")
    return {"packagedHttpPayloads": 3, "webGpuBrowserExecuted": browser}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--browser", action="store_true")
    args = parser.parse_args()
    archive, report = args.archive.resolve(), args.report.resolve()
    if not archive.is_relative_to(ROOT) or not report.is_relative_to(ROOT):
        raise ValueError("release smoke paths must stay in this repository")
    report.parent.mkdir(parents=True, exist_ok=True)
    temporary = ROOT / ".local/tmp"
    temporary.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="extracted-release-", dir=temporary) as folder:
        directory = Path(folder)
        manifest = unpack(archive, directory)
        if manifest["kind"] not in ("native", "web") or manifest["sourceDirty"]:
            raise ValueError("release must have a known kind and clean source provenance")
        expected = os.environ.get("GITHUB_SHA")
        if expected and manifest["sourceCommit"] != expected:
            raise ValueError("release provenance differs from the tested CI source")
        checks = native(directory, manifest, report.parent) if manifest["kind"] == "native" else web(directory, report.parent, args.browser)
        result = {"sourceCommit": manifest["sourceCommit"], "sourceDirty": manifest["sourceDirty"], "kind": manifest["kind"], "checks": checks, "zipSha256": hashlib.sha256(archive.read_bytes()).hexdigest()}
        report.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
        print("EXTRACTED_RELEASE_OK " + json.dumps(result))


if __name__ == "__main__":
    main()
