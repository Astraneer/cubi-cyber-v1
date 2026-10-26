import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("verify_server_scale", Path(__file__).parents[1] / "verify_server_scale.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class ScaleEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.report = self.root / "spec86-123-456.jsonl"
        self.log = self.root / "run.log"
        self.rows = [
            {"kind": "config", "bots": 32, "mobs": 5000, "machines": 10000, "known_water_cells": 100000,
             "delay_ms": 75, "jitter_ms": 20, "loss_percent": 2},
            {"kind": "live_complete", "fluid_visits": 100000, "min_machine_steps": 400},
            {"kind": "sleeping_complete", "ticks": 32, "retained_mobs": 5000, "retained_machines": 10000,
             "runtime_handoff_bytes": 0, "runtime_replay_bytes": 0},
            {"kind": "restart_exact", "world": True, "mob_samples": True, "machine_samples": True},
            {"kind": "per_poll_limits", "max_machine_prepared": 2048, "max_machine_steps": 2048, "max_mob_steps": 1024},
            {"kind": "transport_last_observation", "queue_overflows": 0},
        ] + [{"kind": "peer", "ordinal": n, "sent_bytes": 1, "received_bytes": 1} for n in range(32)]

    def write(self, rows=None):
        self.report.write_text("\n".join(map(json.dumps, self.rows if rows is None else rows)))
        self.log.write_text(f"actual_server_workload={self.report}\n")

    def test_requires_current_test_marker_even_with_stale_success_report(self):
        self.write()
        self.assertEqual(module.verify(self.log, self.root), self.report)
        self.log.write_text("running 0 tests\ntest result: ok. 0 passed\n")
        with self.assertRaises(ValueError):
            module.verify(self.log, self.root)

    def test_rejects_incomplete_scope_recovery_and_foreground_budget_evidence(self):
        for index, key, value in [(0, "known_water_cells", 99999), (0, "loss_percent", 0),
                                  (2, "runtime_replay_bytes", 1), (3, "world", False),
                                  (4, "max_machine_prepared", 4096), (5, "queue_overflows", 1)]:
            rows = copy.deepcopy(self.rows)
            rows[index][key] = value
            self.write(rows)
            with self.subTest(key=key), self.assertRaises(ValueError):
                module.verify(self.log, self.root)

    def test_rejects_missing_or_duplicate_peers_and_truncated_json(self):
        for rows in [self.rows[:-1], self.rows + [self.rows[-1]]]:
            self.write(rows)
            with self.assertRaises(ValueError):
                module.verify(self.log, self.root)
        self.write()
        with self.report.open("a") as stream:
            stream.write("\n{")
        with self.assertRaises(ValueError):
            module.verify(self.log, self.root)
