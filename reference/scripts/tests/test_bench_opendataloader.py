import io
import json
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from bench_opendataloader import (
    _arguments,
    _print_report,
    _run_engine,
    compare_evaluations,
    evaluate_gates,
)


def evaluation(overall, documents, *, missing=0):
    return {
        "metrics": {
            "score": {
                "overall_mean": overall,
                "nid_mean": overall + 0.01,
            },
            "missing_predictions": missing,
        },
        "documents": [
            {
                "document_id": document_id,
                "scores": {"overall": score},
            }
            for document_id, score in documents.items()
        ],
    }


class ComparisonTests(unittest.TestCase):
    def test_reports_metric_and_document_deltas(self):
        baseline = evaluation(0.80, {"a": 0.8, "b": 0.6, "c": 0.7})
        candidate = evaluation(0.82, {"a": 0.9, "b": 0.5, "c": 0.7})

        result = compare_evaluations(baseline, candidate, top=1)

        self.assertAlmostEqual(result["deltas"]["overall_mean"], 0.02)
        self.assertEqual(result["documents"]["improved"], 1)
        self.assertEqual(result["documents"]["regressed"], 1)
        self.assertEqual(result["documents"]["unchanged"], 1)
        self.assertEqual(
            result["documents"]["largest_improvements"][0]["document_id"], "a"
        )
        self.assertEqual(
            result["documents"]["largest_regressions"][0]["document_id"], "b"
        )

    def test_reference_delta_is_reported(self):
        baseline = evaluation(0.80, {})
        candidate = evaluation(0.82, {})
        reference = evaluation(0.81, {})

        result = compare_evaluations(baseline, candidate, reference)

        self.assertAlmostEqual(
            result["candidate_vs_reference"]["overall_mean"], 0.01
        )

    def test_gates_cover_aggregate_document_missing_and_reference(self):
        comparison = compare_evaluations(
            evaluation(0.80, {"a": 0.8}),
            evaluation(0.79, {"a": 0.7}, missing=1),
            evaluation(0.81, {}),
        )

        failures = evaluate_gates(
            comparison,
            min_overall_delta=0.0,
            max_document_regression=0.05,
            max_missing=0,
            require_reference_lead=True,
        )

        self.assertEqual(len(failures), 4)

    def test_regression_gate_is_independent_of_report_limit(self):
        comparison = compare_evaluations(
            evaluation(0.80, {"a": 0.8}),
            evaluation(0.80, {"a": 0.7}),
            top=0,
        )

        failures = evaluate_gates(
            comparison,
            min_overall_delta=0.0,
            max_document_regression=0.05,
            max_missing=0,
            require_reference_lead=False,
        )

        self.assertEqual(len(failures), 1)
        self.assertIn("largest document regression", failures[0])

    def test_report_handles_reference_without_overall_score(self):
        result = compare_evaluations(
            evaluation(0.80, {}),
            evaluation(0.82, {}),
            {"metrics": {"score": {"nid_mean": 0.81}}},
        )

        output = io.StringIO()
        with redirect_stdout(output):
            _print_report(result)

        self.assertIn("Reference overall: n/a; candidate delta: n/a", output.getvalue())

    def test_reference_gate_reports_missing_score_as_unavailable(self):
        comparison = compare_evaluations(
            evaluation(0.80, {}),
            evaluation(0.82, {}),
        )

        failures = evaluate_gates(
            comparison,
            min_overall_delta=0.0,
            max_document_regression=None,
            max_missing=0,
            require_reference_lead=True,
        )

        self.assertEqual(failures, ["reference overall score is unavailable"])

    def test_arguments_reject_negative_counts_and_allow_zero_top(self):
        required = [
            "--bench-dir",
            ".",
            "--baseline",
            "baseline",
            "--candidate",
            "candidate",
        ]
        self.assertEqual(_arguments(required + ["--top", "0"]).top, 0)
        for option in ("--top", "--max-document-regression", "--max-missing"):
            with self.subTest(option=option), redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit):
                    _arguments(required + [option, "-1"])

    def test_arguments_reject_nonfinite_float_thresholds(self):
        required = [
            "--bench-dir",
            ".",
            "--baseline",
            "baseline",
            "--candidate",
            "candidate",
        ]
        for option in ("--min-overall-delta", "--max-document-regression"):
            for value in ("nan", "inf", "-inf"):
                with self.subTest(option=option, value=value), redirect_stderr(
                    io.StringIO()
                ):
                    with self.assertRaises(SystemExit):
                        _arguments(required + [option, value])

    def test_run_engine_clears_stale_predictions_before_parser(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            bench_dir = root / "bench"
            source = bench_dir / "prediction" / "pdf-inspector"
            source.mkdir(parents=True)
            (source / "stale.md").write_text("stale", encoding="utf-8")
            scratch = root / "scratch"
            scratch.mkdir()

            def fake_run(command, *, cwd, env=None):
                if any(part.endswith("pdf_parser.py") for part in command):
                    self.assertFalse(source.exists())
                    (source / "markdown").mkdir(parents=True)
                    (source / "markdown" / "new.md").write_text(
                        "new", encoding="utf-8"
                    )
                else:
                    destination = scratch / "candidate"
                    (destination / "evaluation.json").write_text(
                        json.dumps(evaluation(0.82, {})), encoding="utf-8"
                    )

            with patch("bench_opendataloader._run", side_effect=fake_run):
                result = _run_engine(
                    bench_dir=bench_dir,
                    python=Path("python"),
                    binary=Path("pdf2md"),
                    label="candidate",
                    scratch_root=scratch,
                )

            self.assertEqual(result["metrics"]["score"]["overall_mean"], 0.82)
            self.assertFalse((source / "stale.md").exists())
            self.assertFalse((scratch / "candidate" / "stale.md").exists())


if __name__ == "__main__":
    unittest.main()
