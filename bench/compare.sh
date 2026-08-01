#!/bin/bash
# Runs both harnesses over the same fixtures and prints a per-fixture ratio.
#
# Usage: bench/compare.sh [--filter SUBSTR] [--iters N] [--budget MS]
#
# The two sides are interleaved: each fixture is measured by Rust and then
# immediately by C#, rather than sweeping one implementation and then the
# other. On a shared VM that matters. Sweeping a whole side at a time, the two
# halves can land in different machine states — one run saw memory bandwidth at
# 8.76 GiB/s and the other at 6.39, which is wider than most of the gaps being
# measured. Interleaving puts both sides in the same state, and each fixture
# carries the calibration it was measured under, so any drift that remains is
# visible in the `drift` column rather than folded silently into the ratio.
#
# The ratio is rust_median / csharp_median, so above 1.00 means the C# port is
# ahead.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${TMPDIR:-/tmp}/pdf-inspector-compare"
rm -rf "$OUT"
mkdir -p "$OUT"

CS="$ROOT/src/PdfInspector.Bench/bin/Release/net10.0/pdf-inspector-bench"
RS="$ROOT/bench/rust/target/release/pdf-inspector-bench"

for binary in "$CS" "$RS"; do
  if [ ! -x "$binary" ]; then
    echo "missing $binary — build it first" >&2
    exit 1
  fi
done

FILTER=""
PASSTHROUGH=()
while [ $# -gt 0 ]; do
  case "$1" in
    --filter) FILTER="$2"; shift 2 ;;
    *) PASSTHROUGH+=("$1"); shift ;;
  esac
done

cd "$ROOT"

FIXTURES=()
for path in reference/tests/fixtures/*.pdf; do
  name="$(basename "$path" .pdf)"
  # The encrypted fixture needs a password; neither harness supplies one.
  case "$name" in encrypted-*) continue ;; esac
  if [ -n "$FILTER" ] && [[ "$name" != *"$FILTER"* ]]; then
    continue
  fi
  FIXTURES+=("$name")
done

if [ ${#FIXTURES[@]} -eq 0 ]; then
  echo "no fixtures matched" >&2
  exit 1
fi

for name in "${FIXTURES[@]}"; do
  printf '%-42s' "$name" >&2
  "$RS" --filter "$name" ${PASSTHROUGH[@]+"${PASSTHROUGH[@]}"} \
      --json "$OUT/rust-$name.json" >/dev/null 2>&1
  printf 'rust ' >&2
  "$CS" --filter "$name" ${PASSTHROUGH[@]+"${PASSTHROUGH[@]}"} \
      --json "$OUT/csharp-$name.json" >/dev/null 2>&1
  printf 'c#\n' >&2
done

python3 - "$OUT" <<'PY'
import glob, json, os, sys

out = sys.argv[1]

def load(prefix):
    runs = {}
    for path in glob.glob(os.path.join(out, f"{prefix}-*.json")):
        doc = json.load(open(path))
        for fixture in doc["fixtures"]:
            runs[fixture["name"]] = (fixture, doc["calibration"])
    return runs

rust, cs = load("rust"), load("csharp")

print(f"\n{'fixture':<40} {'rust ms':>10} {'c# ms':>10} {'ratio':>8}  {'drift':>6}  verdict")
print("─" * 92)

wins = losses = 0
worst = []
for name in sorted(rust.keys() & cs.keys()):
    (rf, rc), (cf, cc) = rust[name], cs[name]
    r, c = rf["medianMs"], cf["medianMs"]
    ratio = r / c if c > 0 else float("inf")

    # How far the machine moved between the pair, by the float kernel — the one
    # the layout and table heuristics spend their time on. A ratio is only worth
    # reading when this sits near 1.00.
    drift = cc["floatNsPerOp"] / rc["floatNsPerOp"]

    if ratio >= 1.0:
        verdict, wins = "c# ahead", wins + 1
    else:
        verdict, losses = "RUST AHEAD", losses + 1
        worst.append((ratio, name, r, c))
    print(f"{name:<40} {r:>10.2f} {c:>10.2f} {ratio:>8.2f}  {drift:>6.2f}  {verdict}")

print("─" * 92)
print(f"{wins} fixtures where C# is ahead, {losses} where Rust is")
if worst:
    print("\nremaining gaps, widest first:")
    for ratio, name, r, c in sorted(worst):
        print(f"  {name:<40} rust {r:8.2f}  c# {c:8.2f}   needs {1/ratio:.2f}x")
PY
