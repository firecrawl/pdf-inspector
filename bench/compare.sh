#!/bin/bash
# Runs both harnesses over the same fixtures and prints a per-fixture ratio.
#
# Usage: bench/compare.sh [--filter SUBSTR] [--iters N] [--budget MS]
#
# The ratio is rust_median / csharp_median, so above 1.00 means the C# port is
# ahead. Both sides also print their calibration kernels; if those disagree
# between the two runs the machine moved underneath them and the ratios need
# scaling before they mean anything.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${TMPDIR:-/tmp}/pdf-inspector-compare"
mkdir -p "$OUT"

CS="$ROOT/src/PdfInspector.Bench/bin/Release/net10.0/pdf-inspector-bench"
RS="$ROOT/bench/rust/target/release/pdf-inspector-bench"

for binary in "$CS" "$RS"; do
  if [ ! -x "$binary" ]; then
    echo "missing $binary — build it first" >&2
    exit 1
  fi
done

cd "$ROOT"
echo "── rust ─────────────────────────────────────────────" >&2
"$RS" "$@" --json "$OUT/rust.json" 2>&1 >/dev/null | grep -E "^(cpu|calibration):" >&2
echo "── c# ───────────────────────────────────────────────" >&2
"$CS" "$@" --json "$OUT/csharp.json" 2>&1 >/dev/null | grep -E "^(cpu|calibration):" >&2

python3 - "$OUT/rust.json" "$OUT/csharp.json" <<'PY'
import json, sys

rust = json.load(open(sys.argv[1]))
cs = json.load(open(sys.argv[2]))

rs = {f["name"]: f for f in rust["fixtures"]}
cf = {f["name"]: f for f in cs["fixtures"]}

rc, cc = rust["calibration"], cs["calibration"]
print(f"\ncalibration  int {rc['intNsPerOp']:.3f} vs {cc['intNsPerOp']:.3f} ns/op   "
      f"float {rc['floatNsPerOp']:.3f} vs {cc['floatNsPerOp']:.3f}   "
      f"mem {rc['memGiBPerSec']:.2f} vs {cc['memGiBPerSec']:.2f} GiB/s")

print(f"\n{'fixture':<40} {'rust ms':>10} {'c# ms':>10} {'ratio':>8}  verdict")
print("─" * 82)

wins = losses = 0
worst = []
for name in sorted(rs.keys() & cf.keys()):
    r, c = rs[name]["medianMs"], cf[name]["medianMs"]
    ratio = r / c if c > 0 else float("inf")
    if ratio >= 1.0:
        verdict, wins = "c# ahead", wins + 1
    else:
        verdict, losses = "RUST AHEAD", losses + 1
        worst.append((ratio, name, r, c))
    print(f"{name:<40} {r:>10.2f} {c:>10.2f} {ratio:>8.2f}  {verdict}")

print("─" * 82)
print(f"{wins} fixtures where C# is ahead, {losses} where Rust is")
if worst:
    print("\nremaining gaps, widest first:")
    for ratio, name, r, c in sorted(worst):
        print(f"  {name:<40} rust {r:8.2f}  c# {c:8.2f}   needs {1/ratio:.2f}x")
PY
