# Benchmarks

Two harnesses — one per implementation — that measure the same fixtures with the
same protocol and emit the same JSON, so their numbers can be compared directly.

```bash
# C# (build first: dotnet build -c Release)
./src/PdfInspector.Bench/bin/Release/net10.0/pdf-inspector-bench \
    --iters 5 --budget 3000 --json /tmp/csharp.json

# Rust
cd bench/rust && BENCH_RUSTC_VERSION="$(rustc --version | cut -d' ' -f2)" \
    cargo build --release
./bench/rust/target/release/pdf-inspector-bench \
    --iters 5 --budget 3000 --json /tmp/rust.json
```

Both accept `--fixtures <dir>`, `--filter <substr>`, `--iters`, `--warmup`,
`--budget` and `--json`. The C# harness adds two diagnostic modes:

```bash
pdf-inspector-bench --phases    reference/tests/fixtures/td9264.pdf --iters 8
pdf-inspector-bench --detectors reference/tests/fixtures/td9264.pdf --iters 8
```

`--phases` splits one extraction into parse / cmaps / extract / detect /
markdown with time and allocation for each. `--detectors` times the three table
detectors over a document's already-extracted geometry, which separates detector
cost from the pipeline around it. Both exist because the pipeline runs table
detection twice — the extractor needs it to tell a table's ruling lines from a
text underline — so a stage timing alone cannot say where the cost sits.

## Why in-process

Timing the CLI measures process start, assembly load and JIT far more than it
measures the extractor, and the distortion is not small: `bits_pilani_feedback`
takes about 15 s of steady-state work but roughly 190 s as a single cold CLI
pass, because a one-shot run never leaves tier-0 code. Both harnesses therefore
warm up until the hot paths are compiled and the caches are settled, then time
repeated runs in-process and report the distribution.

## Why warmup is five seconds

Long enough to look like overkill, and it isn't. .NET promotes a method to
tier-1 by counting its invocations, and one extraction calls most of the
pipeline's per-document methods exactly once — so a short warmup finishes with
much of the pipeline still in tier-0 code. Run `td9264` alone under a
three-pass warmup and it reports 84 ms; run it after other fixtures have warmed
the shared code and the same work reports 20 ms.

Nothing in the timings marks the moment the promotions land, so a convergence
rule — stop once five consecutive passes fail to improve — does not fix it: it
stops at pass 12, still reading 110 ms, because tier-up is a delayed step down
rather than a glide.

A fixed pass count does not fix it either, because how many passes it takes
scales with how small the document is:

| passes | `td9264` | `wireless_two_col_no_rects` |
|---|---|---|
| 100 | 22.6 ms | 13.4 ms |
| 300 | 18.4 ms | — |
| 400 | — | 4.9 ms |
| 1000 | — | 4.0 ms |
| 1750 | — | 4.0 ms |

What the two have in common is the wall clock: both settle after roughly four
seconds of repetition. So that is the rule — repeat for
`max(budget, 5 s)` — and it needs no cap. A fixture whose single pass already
takes seconds satisfies it after one or two, which is all such a fixture needs:
its time is inside loops, which on-stack replacement promotes without waiting
for a call count. `multiline_indent_cell_rect_grid` reads 1649 ms at 3 warmups
and 1690 ms at 100.

The Rust harness runs the identical rule. It has no JIT, so the repetitions buy
only settled caches and allocator free lists, but a protocol that differs
between the two sides is not a comparison.

## The calibration kernels

Cloud VMs move between host generations and throttle under contention, so a
millisecond figure from one session is not comparable with another on its own.
Every run therefore begins with three fixed kernels and prints a CPU line:

```
cpu: Intel(R) Xeon(R) Processor @ 2.80GHz | 4 logical cores | AVX-512F+BW | .NET 10.0.10
calibration: int 2.769 ns/op | float 3.991 ns/op | mem 7.03 GiB/s
```

- **int** — a xorshift64\* chain. Strictly sequential, so neither compiler can
  vectorise it: this is scalar ALU latency and effective clock.
- **float** — a scalar f32 multiply-add chain. Floating-point latency, which is
  what the layout and table heuristics spend their time on.
- **mem** — a streaming sum over a buffer far larger than L2, unrolled across
  eight independent accumulators. Memory bandwidth.

To compare two runs, divide their kernel rates to get the factor the machine
changed by, then apply it to the fixture timings.

The kernels are defined twice — `src/PdfInspector.Bench/MachineProfile.cs` and
`bench/rust/src/kernels.rs` — and **must stay byte-for-byte equivalent**: same
constants, same iteration counts, same arithmetic. Changing one side without the
other silently breaks cross-implementation scaling.

The unrolling in the memory kernel is load-bearing. The first draft accumulated
into a single variable, which left each add waiting on the previous one, so the
kernel measured whichever compiler vectorised more aggressively rather than the
machine: 7.05 GiB/s under Rust against 3.89 GiB/s under .NET on the same CPU.
Eight independent chains saturate the load ports either way. The int and float
kernels agree to within about 1% and 5% respectively across the two toolchains,
which is what a machine fingerprint should look like.

## Layout

```
bench/rust/          Rust harness — a separate crate with a path dependency on
                     reference/, so the reference crate stays verbatim
src/PdfInspector.Bench/   C# harness
```
