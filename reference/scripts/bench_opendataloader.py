#!/usr/bin/env python3
"""Run a paired pdf-inspector OpenDataLoader benchmark and report deltas."""

from __future__ import annotations

import argparse
import json
import math
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any


SCORE_KEYS = (
    "overall_mean",
    "nid_mean",
    "nid_s_mean",
    "teds_mean",
    "teds_s_mean",
    "mhs_mean",
    "mhs_s_mean",
)


def _non_negative_int(value: str) -> int:
    parsed = int(value)
    if parsed < 0:
        raise argparse.ArgumentTypeError("must be non-negative")
    return parsed


def _non_negative_float(value: str) -> float:
    parsed = float(value)
    if not math.isfinite(parsed) or parsed < 0.0:
        raise argparse.ArgumentTypeError("must be finite and non-negative")
    return parsed


def _finite_float(value: str) -> float:
    parsed = float(value)
    if not math.isfinite(parsed):
        raise argparse.ArgumentTypeError("must be finite")
    return parsed


def _scores(evaluation: dict[str, Any]) -> dict[str, float]:
    score = evaluation.get("metrics", {}).get("score", {})
    return {key: float(score[key]) for key in SCORE_KEYS if score.get(key) is not None}


def _documents(evaluation: dict[str, Any]) -> dict[str, float]:
    documents: dict[str, float] = {}
    for document in evaluation.get("documents", []):
        overall = document.get("scores", {}).get("overall")
        if overall is not None:
            documents[str(document["document_id"])] = float(overall)
    return documents


def compare_evaluations(
    baseline: dict[str, Any],
    candidate: dict[str, Any],
    reference: dict[str, Any] | None = None,
    *,
    top: int = 10,
) -> dict[str, Any]:
    """Build aggregate and per-document deltas from evaluator JSON payloads."""
    baseline_scores = _scores(baseline)
    candidate_scores = _scores(candidate)
    metric_deltas = {
        key: candidate_scores[key] - baseline_scores[key]
        for key in SCORE_KEYS
        if key in baseline_scores and key in candidate_scores
    }

    baseline_documents = _documents(baseline)
    candidate_documents = _documents(candidate)
    shared = sorted(baseline_documents.keys() & candidate_documents.keys())
    document_deltas = [
        {
            "document_id": document_id,
            "baseline": baseline_documents[document_id],
            "candidate": candidate_documents[document_id],
            "delta": candidate_documents[document_id] - baseline_documents[document_id],
        }
        for document_id in shared
    ]
    epsilon = 1e-12
    improvements = sorted(document_deltas, key=lambda item: item["delta"], reverse=True)
    regressions = sorted(document_deltas, key=lambda item: item["delta"])

    result: dict[str, Any] = {
        "baseline": baseline_scores,
        "candidate": candidate_scores,
        "deltas": metric_deltas,
        "missing_predictions": {
            "baseline": int(baseline.get("metrics", {}).get("missing_predictions", 0)),
            "candidate": int(candidate.get("metrics", {}).get("missing_predictions", 0)),
        },
        "documents": {
            "shared": len(shared),
            "improved": sum(item["delta"] > epsilon for item in document_deltas),
            "regressed": sum(item["delta"] < -epsilon for item in document_deltas),
            "unchanged": sum(abs(item["delta"]) <= epsilon for item in document_deltas),
            "largest_improvements": [
                item for item in improvements if item["delta"] > epsilon
            ][:top],
            "largest_regressions": [
                item for item in regressions if item["delta"] < -epsilon
            ][:top],
            "worst_regression": next(
                (item for item in regressions if item["delta"] < -epsilon), None
            ),
        },
    }
    if reference is not None:
        reference_scores = _scores(reference)
        result["reference"] = reference_scores
        result["candidate_vs_reference"] = {
            key: candidate_scores[key] - reference_scores[key]
            for key in SCORE_KEYS
            if key in candidate_scores and key in reference_scores
        }
    return result


def evaluate_gates(
    comparison: dict[str, Any],
    *,
    min_overall_delta: float,
    max_document_regression: float | None,
    max_missing: int,
    require_reference_lead: bool,
) -> list[str]:
    """Return human-readable gate failures; an empty list means pass."""
    failures: list[str] = []
    overall_delta = comparison["deltas"].get("overall_mean")
    if overall_delta is None or overall_delta < min_overall_delta:
        failures.append(
            f"overall delta {overall_delta!r} is below {min_overall_delta:+.6f}"
        )
    candidate_missing = comparison["missing_predictions"]["candidate"]
    if candidate_missing > max_missing:
        failures.append(
            f"candidate has {candidate_missing} missing predictions (maximum {max_missing})"
        )
    if max_document_regression is not None:
        regression = comparison["documents"].get("worst_regression")
        if regression is not None and regression["delta"] < -max_document_regression:
            failures.append(
                "largest document regression "
                f"{regression['document_id']}={regression['delta']:+.6f} "
                f"exceeds {-max_document_regression:+.6f}"
            )
    if require_reference_lead:
        reference_delta = comparison.get("candidate_vs_reference", {}).get("overall_mean")
        if reference_delta is None:
            failures.append("reference overall score is unavailable")
        elif reference_delta < 0.0:
            failures.append(
                f"candidate trails reference overall by {reference_delta!r}"
            )
    return failures


def _run(command: list[str], *, cwd: Path, env: dict[str, str] | None = None) -> None:
    print("+", " ".join(command), flush=True)
    subprocess.run(command, cwd=cwd, env=env, check=True)


def _run_engine(
    *,
    bench_dir: Path,
    python: Path,
    binary: Path,
    label: str,
    scratch_root: Path,
) -> dict[str, Any]:
    env = os.environ.copy()
    env["PDF_INSPECTOR_BINARY"] = str(binary)
    source = bench_dir / "prediction" / "pdf-inspector"
    if source.exists():
        if source.is_dir():
            shutil.rmtree(source)
        else:
            source.unlink()
    _run(
        [
            str(python),
            "src/pdf_parser.py",
            "--engine",
            "pdf-inspector",
            "--log-level",
            "WARNING",
        ],
        cwd=bench_dir,
        env=env,
    )

    if not source.is_dir():
        raise RuntimeError(f"parser did not produce predictions: {source}")
    destination = scratch_root / label
    shutil.copytree(source, destination)
    _run(
        [
            str(python),
            "src/evaluator.py",
            "--prediction-root",
            str(scratch_root),
            "--engine",
            label,
            "--log-level",
            "WARNING",
        ],
        cwd=bench_dir,
    )
    with (destination / "evaluation.json").open(encoding="utf-8") as handle:
        return json.load(handle)


def _print_report(comparison: dict[str, Any]) -> None:
    print("\nMetric                 baseline   candidate       delta")
    print("--------------------  ----------  ----------  ----------")
    for key in SCORE_KEYS:
        if key not in comparison["deltas"]:
            continue
        print(
            f"{key:<20}  {comparison['baseline'][key]:>10.6f}  "
            f"{comparison['candidate'][key]:>10.6f}  "
            f"{comparison['deltas'][key]:>+10.6f}"
        )
    if "reference" in comparison:
        delta = comparison["candidate_vs_reference"].get("overall_mean")
        reference = comparison["reference"].get("overall_mean")
        reference_display = f"{reference:.6f}" if reference is not None else "n/a"
        delta_display = f"{delta:+.6f}" if delta is not None else "n/a"
        print(f"\nReference overall: {reference_display}; candidate delta: {delta_display}")

    documents = comparison["documents"]
    print(
        "\nDocuments: "
        f"{documents['improved']} improved, {documents['regressed']} regressed, "
        f"{documents['unchanged']} unchanged ({documents['shared']} shared)"
    )
    for heading, key in (
        ("Largest improvements", "largest_improvements"),
        ("Largest regressions", "largest_regressions"),
    ):
        print(f"\n{heading}:")
        rows = documents[key]
        if not rows:
            print("  none")
        for row in rows:
            print(
                f"  {row['document_id']}: {row['delta']:+.6f} "
                f"({row['baseline']:.6f} -> {row['candidate']:.6f})"
            )


def _arguments(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bench-dir", type=Path, required=True)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--python", type=Path)
    parser.add_argument("--reference-evaluation", type=Path)
    parser.add_argument("--json-output", type=Path)
    parser.add_argument("--top", type=_non_negative_int, default=10)
    parser.add_argument("--min-overall-delta", type=_finite_float, default=0.0)
    parser.add_argument("--max-document-regression", type=_non_negative_float)
    parser.add_argument("--max-missing", type=_non_negative_int, default=0)
    parser.add_argument("--require-reference-lead", action="store_true")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = _arguments(argv)
    bench_dir = args.bench_dir.resolve()
    baseline = args.baseline.resolve()
    candidate = args.candidate.resolve()
    # Keep the virtualenv launcher path intact. Resolving its symlink would
    # invoke the underlying system interpreter without the benchmark's site
    # packages.
    python = (args.python or bench_dir / ".venv" / "bin" / "python").absolute()
    for path, description in (
        (bench_dir / "src" / "pdf_parser.py", "OpenDataLoader parser"),
        (bench_dir / "src" / "evaluator.py", "OpenDataLoader evaluator"),
        (baseline, "baseline binary"),
        (candidate, "candidate binary"),
        (python, "Python interpreter"),
    ):
        if not path.exists():
            raise SystemExit(f"{description} not found: {path}")

    with tempfile.TemporaryDirectory(prefix="pdf-inspector-opendataloader-") as temporary:
        scratch_root = Path(temporary)
        baseline_evaluation = _run_engine(
            bench_dir=bench_dir,
            python=python,
            binary=baseline,
            label="baseline",
            scratch_root=scratch_root,
        )
        candidate_evaluation = _run_engine(
            bench_dir=bench_dir,
            python=python,
            binary=candidate,
            label="candidate",
            scratch_root=scratch_root,
        )

        reference = None
        if args.reference_evaluation is not None:
            with args.reference_evaluation.resolve().open(encoding="utf-8") as handle:
                reference = json.load(handle)

        comparison = compare_evaluations(
            baseline_evaluation,
            candidate_evaluation,
            reference,
            top=args.top,
        )

    _print_report(comparison)
    if args.json_output is not None:
        args.json_output.resolve().write_text(
            json.dumps(comparison, indent=2) + "\n", encoding="utf-8"
        )

    failures = evaluate_gates(
        comparison,
        min_overall_delta=args.min_overall_delta,
        max_document_regression=args.max_document_regression,
        max_missing=args.max_missing,
        require_reference_lead=args.require_reference_lead,
    )
    if failures:
        print("\nBenchmark gate failed:", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1
    print("\nBenchmark gate passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
