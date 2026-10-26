#!/usr/bin/env python3
"""Emit bounded compiler/backend failure details as public Actions annotations."""
import os
from collections import deque
from pathlib import Path
import re
import sys


def diagnostic(text):
    for name in ("GH_TOKEN", "GITHUB_TOKEN"):
        value = os.environ.get(name)
        if value:
            text = text.replace(value, "[redacted]")
    lines = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", text).splitlines()
    # Pixel equality failures can print megabytes on one left/right line. Keep
    # the panic location and both prefixes instead of letting the array consume
    # the entire annotation or push the panic outside a tail-only log read.
    lines = [line if len(line) <= 320 else line[:240] + f" ... [{len(line)} chars] ... " + line[-64:] for line in lines]
    selected = set()
    for index, line in enumerate(lines):
        if re.search(r"(?i)(panicked at|^\s*error(?:\[[^]]+\])?:|^\s*caused by|validation error|stack overflow|failed to (?:open|load|create)|xopendisplay|error while loading|could not be loaded)", line):
            selected.update(range(index, min(index + 5, len(lines))))
    if not selected:
        selected.update(range(max(0, len(lines) - 8), len(lines)))
    result = " | ".join(lines[i].strip() for i in sorted(selected) if lines[i].strip())
    return result.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")[-1800:]


def read_diagnostic(path):
    lines = deque(maxlen=8192)
    with path.open("rb") as stream:
        while prefix := stream.readline(8192):
            tail = prefix
            skipped = 0
            while tail and not tail.endswith(b"\n"):
                tail = stream.readline(8192)
                skipped += len(tail)
            line = prefix.decode("utf-8", errors="replace").rstrip("\r\n")
            if skipped:
                line += f" ... [{skipped} further bytes]"
            lines.append(line)
    return diagnostic("\n".join(lines))


def main():
    path = Path(sys.argv[1])
    # Bounds apply to the diagnostic reader, not to the retained complete log.
    result = read_diagnostic(path)
    if result:
        print(f"::error title=Acceptance diagnostic::{result}")


if __name__ == "__main__":
    main()
