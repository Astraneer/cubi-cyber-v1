from copy import deepcopy
import json
from pathlib import Path
import subprocess
import sys
import tempfile
from unittest import TestCase

from tools.bench_report import compatible, load, percentile, render, summarize, validate

ROOT = Path(__file__).resolve().parents[2]


def fixture():
    frames = []
    for index, interval in enumerate((10, 20, 30, 40, 50)):
        frames.append({"frame": index + 1, "timeNs": index * 100_000_000,
                       "intervalNs": interval * 1_000_000, "cpuNs": 5_000_000,
                       "gpuNs": None, "cpu": [None] * 8, "gpu": [None] * 16,
                       "observations": 8 if index == 4 else 0,
                       "queuedJobs": 0, "oldestJobTicks": 0, "inFlight": 1,
                       "allocations": None, "work": {"cells": 100, "quads": 20,
                                                       "uploads": 64, "pipelines": 0,
                                                       "syncGeneration": 0}})
    return {"schemaVersion": 1, "backend": "Synthetic test fixture (not measured performance)",
            "completeWindow": False, "lostFrames": 0, "lostInputs": 0,
            "windowNs": 10_000_000_000, "publicSeed": "0000000000000539",
            "frames": frames, "benchmark": {"route": "fixture-v1", "width": 320,
                                           "height": 224, "presentationIncluded": False}}


class BenchReportTests(TestCase):
    def test_known_percentiles_and_unknown_gpu_are_distinct(self):
        summary = summarize(fixture())
        self.assertEqual(summary["frame"]["p50"], 30)
        self.assertEqual(summary["frame"]["p95"], 50)
        self.assertEqual(summary["gpu"]["samples"], 0)
        self.assertIsNone(summary["gpu"]["p50"])
        self.assertEqual(summary["work"]["cells"], 500)
        self.assertEqual(summary["observations"]["Бюджет загрузки"], 1)
        self.assertEqual(summary["allocationSamples"], 0)
        self.assertEqual(summary["hitches25"], 3)
        self.assertEqual(summary["hitches50"], 0)
        self.assertIsNone(percentile([], .99))

    def test_comparison_requires_same_route_adapter_and_size(self):
        first, second = fixture(), fixture()
        self.assertTrue(compatible(first, second))
        second["benchmark"]["width"] = 640
        self.assertFalse(compatible(first, second))
        second = fixture()
        second["backend"] = "another adapter"
        self.assertFalse(compatible(first, second))
        second = fixture()
        second["frames"].pop()
        self.assertFalse(compatible(first, second))
        second = fixture()
        second["initialHash"] = "other world"
        self.assertFalse(compatible(first, second))
        del first["benchmark"]
        self.assertFalse(compatible(first, fixture()))

    def test_bad_order_shapes_and_non_integer_measurements_fail(self):
        for field, value in (("timeNs", -1), ("inFlight", 3), ("cpuNs", True),
                             ("gpuNs", float("inf")), ("frame", 0)):
            record = fixture()
            record["frames"][1][field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                validate(record)
        record = fixture()
        record["frames"][0]["gpu"] = []
        with self.assertRaises(ValueError):
            validate(record)

    def test_html_escapes_record_content_in_both_markup_and_json(self):
        record = fixture()
        record["backend"] = '</script><img src=x onerror="alert(1)">'
        page = render([record])
        self.assertNotIn(record["backend"], page)
        self.assertIn("\\u003c/script\\u003e", page)
        self.assertIn("&lt;img", page)
        self.assertIn("неполное", page)
        self.assertIn("не измерено", page)

    def test_real_cli_writes_standalone_comparison_and_rejects_nan(self):
        cache = ROOT / ".local/tmp"
        cache.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=cache) as directory:
            directory = Path(directory)
            first, second = directory / "a.json", directory / "b.json"
            first.write_text(json.dumps(fixture()))
            changed = deepcopy(fixture())
            for frame in changed["frames"]:
                frame["intervalNs"] //= 2
            second.write_text(json.dumps(changed))
            output = directory / "comparison.html"
            result = subprocess.run([sys.executable, str(ROOT / "tools/bench_report.py"),
                                     "compare", str(first), str(second), "--out", str(output)],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("-50.000 %", output.read_text())
            self.assertIn("BENCH_REPORT_OK", result.stdout)
            first.write_text(first.read_text().replace('"cpuNs": 5000000', '"cpuNs": NaN'))
            with self.assertRaises(ValueError):
                load(first)

    def test_complete_gpu_spans_and_partial_pass_sums_are_not_mixed_or_compared(self):
        complete = fixture()
        for frame in complete["frames"]:
            frame["gpuNs"] = 8_000_000
            frame["gpuTotalComplete"] = True
        partial = deepcopy(complete)
        for frame in partial["frames"]:
            frame.pop("gpuTotalComplete")
            frame["gpuNs"] = 2_000_000
        self.assertTrue(compatible(complete, deepcopy(complete)))
        self.assertFalse(compatible(complete, partial))
        mixed = deepcopy(complete)
        mixed["frames"][0].pop("gpuTotalComplete")
        mixed["frames"][0]["gpuNs"] = 1_000_000
        result = summarize(mixed)
        self.assertEqual(result["gpu"]["samples"], 4)
        self.assertEqual(result["gpu"]["p50"], 8)
        self.assertEqual(result["gpuPartial"]["samples"], 1)
        self.assertIsNone(result["timeline"][0][3])
        self.assertFalse(compatible(mixed, deepcopy(mixed)))
        self.assertIn("показаны раздельно", render([mixed]))
        bad = fixture()
        bad["frames"][0]["gpuTotalComplete"] = True
        with self.assertRaises(ValueError):
            validate(bad)
