#!/usr/bin/env python3
"""Emit bounded compiler/backend failure details as public Actions annotations."""
import os
from pathlib import Path
import re
import sys


def diagnostic(text):
    lines = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", text).splitlines()
    selected = set()
    for index, line in enumerate(lines):
        if re.search(r"(?i)(panicked at|^\s*error(?:\[[^]]+\])?:|^\s*caused by|validation error|stack overflow|failed to (?:open|load|create)|xopendisplay|error while loading|could not be loaded)", line):
            selected.update(range(index, min(index + 5, len(lines))))
    if not selected:
        selected.update(range(max(0, len(lines) - 8), len(lines)))
    result = " | ".join(lines[i].strip() for i in sorted(selected) if lines[i].strip())
    for name in ("GH_TOKEN", "GITHUB_TOKEN"):
        value = os.environ.get(name)
        if value:
            result = result.replace(value, "[redacted]")
    return result.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")[-1800:]


def main():
    path = Path(sys.argv[1])
    # Bounds apply to the diagnostic reader, not to the retained complete log.
    with path.open("rb") as stream:
        stream.seek(max(0, path.stat().st_size - 256 * 1024))
        result = diagnostic(stream.read().decode("utf-8", errors="replace"))
    if result:
        print(f"::error title=Acceptance diagnostic::{result}")


if __name__ == "__main__":
    main()
