#!/usr/bin/env python3
"""Synthetic acceptance fixtures for the soak capture-cost recording."""

import copy
import math
import pathlib
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

from soak import Metrics, capture_verdict, terminated_lines  # noqa: E402


SOURCE_ID = "9cc45a72-a4a4-4cae-b64c-f921a94bc505"


def valid_recording() -> dict:
    return {
        "thread_cpu_clock_available": True,
        "measured_source_id": SOURCE_ID,
        "sources": [
            {
                "source_id": "another-active-source",
                "name": "larger command",
                "journal_bytes": 400 * 1048576,
                "records": 4_000_000,
                "commits": 200,
                "handovers": 4_000,
                "writer_cpu_seconds": 1.0,
                "reader_cpu_seconds": 1.0,
            },
            {
                "source_id": SOURCE_ID,
                "name": "soak.log",
                "journal_bytes": 108 * 1048576,
                "records": 620_000,
                "commits": 129,
                "handovers": 363,
                "writer_cpu_seconds": 1.0,
                "reader_cpu_seconds": 0.1,
            },
        ],
    }


def summary(recording: dict) -> dict:
    metrics = Metrics()
    metrics.capture(64 * 1048576, 31.012345, 4.0)
    metrics.capture_sources(recording)
    return metrics.capture_summary()


class CaptureVerdictTests(unittest.TestCase):
    def test_fixture_completion_counts_only_terminated_lines(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = pathlib.Path(directory) / "fixture.log"
            fixture.write_bytes(b"one\ntwo\npartial")
            self.assertEqual(terminated_lines(fixture), 2)

    def test_selects_generated_source_by_stable_identity(self) -> None:
        capture = summary(valid_recording())
        self.assertEqual(capture["measurement_status"], "valid")
        self.assertEqual(capture["measured_source_ids"], [SOURCE_ID])
        self.assertEqual(capture_verdict(capture), [])
        self.assertEqual(capture["journal_mib_per_writer_cpu_second"], 108.0)

    def test_missing_identity_fails_acceptance(self) -> None:
        recording = valid_recording()
        recording.pop("measured_source_id")
        capture = summary(recording)
        self.assertEqual(capture["measurement_status"], "invalid")
        self.assertTrue(capture_verdict(capture))

    def test_zero_clock_value_fails_acceptance(self) -> None:
        recording = valid_recording()
        recording["sources"][1]["writer_cpu_seconds"] = 0.0
        self.assertTrue(capture_verdict(summary(recording)))

    def test_missing_probe_field_fails_acceptance(self) -> None:
        recording = valid_recording()
        recording["sources"][1].pop("handovers")
        self.assertTrue(capture_verdict(summary(recording)))

    def test_malformed_sources_fails_acceptance(self) -> None:
        recording = valid_recording()
        recording["sources"] = {"not": "a list"}
        self.assertTrue(capture_verdict(summary(recording)))

    def test_nonfinite_and_negative_fields_fail_acceptance(self) -> None:
        for field, value in [("reader_cpu_seconds", math.nan), ("records", -1)]:
            with self.subTest(field=field):
                recording = copy.deepcopy(valid_recording())
                recording["sources"][1][field] = value
                self.assertTrue(capture_verdict(summary(recording)))

    def test_zero_derived_rate_cannot_pass_truthiness_guard(self) -> None:
        capture = summary(valid_recording())
        capture["mib_per_reader_plus_writer_cpu_second"] = 0.0
        self.assertTrue(capture_verdict(capture))

    def test_near_boundary_rates_are_not_rounded_into_a_pass(self) -> None:
        recording = valid_recording()
        measured = recording["sources"][1]
        measured["journal_bytes"] = 1000 * 1048576
        measured["commits"] = 6004
        measured["writer_cpu_seconds"] = 10.0
        measured["reader_cpu_seconds"] = 15.0

        metrics = Metrics()
        metrics.capture(699 * 1048576, 31.012345, 30.0)
        metrics.capture_sources(recording)
        capture = metrics.capture_summary()

        self.assertEqual(capture["mib_per_reader_plus_writer_cpu_second"], 27.96)
        self.assertEqual(capture["commits_per_journal_mib"], 6.004)
        failures = capture_verdict(capture)
        self.assertTrue(any("reader+writer" in failure for failure in failures))
        self.assertTrue(any("committed" in failure for failure in failures))

    def test_each_restarted_process_discards_its_own_cold_rss_sample(self) -> None:
        metrics = Metrics()
        metrics.samples = [
            {"instance": 0, "rss_mib": value}
            for value in [127.0, 151.8, 152.1, 69.2]
        ] + [
            {"instance": 1, "rss_mib": value}
            for value in [72.0, 125.3, 138.9]
        ]
        self.assertEqual(
            metrics.summary()["rss_growth_by_instance"],
            [
                {"instance": 0, "steady_rss_mib": [151.8, 152.1, 69.2], "growth_mib": 0.0},
                {"instance": 1, "steady_rss_mib": [125.3, 138.9], "growth_mib": 13.6},
            ],
        )


if __name__ == "__main__":
    unittest.main()
