#!/usr/bin/env python3
"""Fail closed if the required ignored test did not produce complete evidence."""
import json
from pathlib import Path
import re
import sys


def verify(log, artifact_root):
    paths = re.findall(r"^actual_server_workload=(.+)$", Path(log).read_text(), re.MULTILINE)
    if len(paths) != 1:
        raise ValueError("expected one actually executed server workload")
    report = Path(paths[0]).resolve()
    if report.parent != Path(artifact_root).resolve() or not re.fullmatch(r"spec86-[0-9]+-[0-9]+\.jsonl", report.name):
        raise ValueError("workload report is outside the artifact directory")
    if report.stat().st_size > 64 * 1024 * 1024:
        raise ValueError("workload report exceeds bounded evidence size")
    rows = [json.loads(line) for line in report.read_text().splitlines()]

    def one(kind):
        values = [row for row in rows if row.get("kind") == kind]
        if len(values) != 1:
            raise ValueError(f"expected one {kind} record")
        return values[0]

    expected = {"bots": 32, "mobs": 5000, "machines": 10000, "known_water_cells": 100000,
                "delay_ms": 75, "jitter_ms": 20, "loss_percent": 2}
    config = one("config")
    if any(config.get(key) != value for key, value in expected.items()):
        raise ValueError("required workload or network profile changed")
    live = one("live_complete")
    if live.get("fluid_visits") != 100000 or live.get("min_machine_steps", 0) <= 0:
        raise ValueError("active owner work did not complete")
    cold = one("sleeping_complete")
    if any(cold.get(key) != value for key, value in {"ticks": 32, "retained_mobs": 5000,
            "retained_machines": 10000, "runtime_handoff_bytes": 0, "runtime_replay_bytes": 0}.items()):
        raise ValueError("sleeping owner evidence is incomplete")
    if any(one("restart_exact").get(key) is not True for key in ("world", "mob_samples", "machine_samples")):
        raise ValueError("actual store recovery was not verified")
    peaks = one("per_poll_limits")
    if any(not 0 < peaks.get(key, 0) <= bound for key, bound in
           {"max_machine_prepared": 2048, "max_machine_steps": 2048, "max_mob_steps": 1024}.items()):
        raise ValueError("foreground owner portion exceeded its bound")
    if one("transport_last_observation").get("queue_overflows") != 0:
        raise ValueError("transport queue overflow")
    peers = [row for row in rows if row.get("kind") == "peer"]
    if len(peers) != 32 or {row.get("ordinal") for row in peers} != set(range(32)):
        raise ValueError("missing actual UDP peers")
    if any(row.get("sent_bytes", 0) <= 0 or row.get("received_bytes", 0) <= 0 for row in peers):
        raise ValueError("peer traffic was not observed")
    return report


if __name__ == "__main__":
    try:
        result = verify(sys.argv[1], Path(__file__).resolve().parents[1] / ".local/artifacts")
    except (ValueError, OSError, IndexError, TypeError) as error:
        raise SystemExit(f"SPEC86 evidence rejected: {error}") from None
    print(f"SPEC86_REPORT_OK {result.name}")
