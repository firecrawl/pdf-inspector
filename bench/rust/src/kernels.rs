//! Machine-speed kernels.
//!
//! These must stay byte-for-byte equivalent to
//! `src/PdfInspector.Bench/MachineProfile.cs` — same constants, same iteration
//! counts, same arithmetic. They exist so a run on one VM can be scaled into
//! agreement with a run on another: divide the two runs' kernel rates to get
//! the factor the machine changed by, then apply it to the fixture timings.
//! Changing one side without the other silently breaks that.

use std::hint::black_box;
use std::time::Instant;

pub const INT_KERNEL_ITERATIONS: u32 = 50_000_000;
pub const FLOAT_KERNEL_ITERATIONS: u32 = 50_000_000;
pub const MEM_KERNEL_BYTES: usize = 32 << 20;
pub const MEM_KERNEL_PASSES: usize = 20;

/// A xorshift64* chain. Every step depends on the previous one, so neither
/// compiler can vectorise or reorder it — this measures scalar ALU latency and
/// effective clock, nothing else.
#[inline(never)]
pub fn int_kernel(iterations: u32) -> u64 {
    let mut x: u64 = 0x2545F4914F6CDD1D;
    for _ in 0..iterations {
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        x = x.wrapping_mul(0x2545F4914F6CDD1D);
    }
    x
}

/// A scalar f32 multiply-add chain, again strictly sequential. Measures
/// floating-point latency, which is what the layout and table heuristics spend
/// their time on.
#[inline(never)]
pub fn float_kernel(iterations: u32) -> f32 {
    let mut acc: f32 = 1.0;
    for _ in 0..iterations {
        acc = acc * 1.0000001 + 0.000001;
        if acc > 1e18 {
            acc = 1.0;
        }
    }
    acc
}

/// A streaming sum over a buffer far larger than L2, unrolled across eight
/// independent accumulators.
///
/// The unrolling is what makes this a memory measurement rather than a codegen
/// one. A plain accumulate loop leaves each add waiting on the previous one, so
/// whichever compiler vectorises more aggressively wins by a factor that has
/// nothing to do with the machine — the first draft of this kernel read
/// 7.05 GiB/s here and 3.89 GiB/s under .NET on the same CPU for exactly that
/// reason. Eight independent chains saturate the load ports either way, so both
/// sides end up bandwidth-bound.
#[inline(never)]
pub fn mem_kernel(buffer: &[u64], passes: usize) -> u64 {
    let mut a: [u64; 8] = [0; 8];
    let length = buffer.len() - (buffer.len() % 8);

    for _ in 0..passes {
        let mut i = 0;
        while i < length {
            a[0] = a[0].wrapping_add(buffer[i]);
            a[1] = a[1].wrapping_add(buffer[i + 1]);
            a[2] = a[2].wrapping_add(buffer[i + 2]);
            a[3] = a[3].wrapping_add(buffer[i + 3]);
            a[4] = a[4].wrapping_add(buffer[i + 4]);
            a[5] = a[5].wrapping_add(buffer[i + 5]);
            a[6] = a[6].wrapping_add(buffer[i + 6]);
            a[7] = a[7].wrapping_add(buffer[i + 7]);
            i += 8;
        }
    }

    a.iter().fold(0u64, |acc, v| acc.wrapping_add(*v))
}

#[derive(Debug, Clone, Copy)]
pub struct Calibration {
    pub int_ns_per_op: f64,
    pub float_ns_per_op: f64,
    pub mem_gib_per_sec: f64,
}

impl Calibration {
    pub fn to_json(self) -> String {
        format!(
            "{{\"intNsPerOp\":{:.6},\"floatNsPerOp\":{:.6},\"memGiBPerSec\":{:.6}}}",
            self.int_ns_per_op, self.float_ns_per_op, self.mem_gib_per_sec
        )
    }

    pub fn describe(self) -> String {
        format!(
            "int {:.3} ns/op | float {:.3} ns/op | mem {:.2} GiB/s",
            self.int_ns_per_op, self.float_ns_per_op, self.mem_gib_per_sec
        )
    }
}

pub fn calibrate() -> Calibration {
    // Short warm passes so caches and branch predictors are in the same state
    // the C# harness reaches after its tier-up warmup.
    black_box(int_kernel(INT_KERNEL_ITERATIONS / 50));
    black_box(float_kernel(FLOAT_KERNEL_ITERATIONS / 50));

    let mut buffer = vec![0u64; MEM_KERNEL_BYTES / std::mem::size_of::<u64>()];
    for (i, slot) in buffer.iter_mut().enumerate() {
        *slot = (i as u64).wrapping_mul(0x9E3779B97F4A7C15);
    }
    black_box(mem_kernel(&buffer, 1));

    let t = Instant::now();
    let int_sink = int_kernel(INT_KERNEL_ITERATIONS);
    let int_ns = t.elapsed().as_secs_f64() * 1e9 / f64::from(INT_KERNEL_ITERATIONS);

    let t = Instant::now();
    let float_sink = float_kernel(FLOAT_KERNEL_ITERATIONS);
    let float_ns = t.elapsed().as_secs_f64() * 1e9 / f64::from(FLOAT_KERNEL_ITERATIONS);

    let t = Instant::now();
    let mem_sink = mem_kernel(&buffer, MEM_KERNEL_PASSES);
    let mem_seconds = t.elapsed().as_secs_f64();
    let mem_gib = (MEM_KERNEL_BYTES * MEM_KERNEL_PASSES) as f64 / (1u64 << 30) as f64;

    // Keep the results observable so nothing is deleted as dead code.
    black_box(int_sink);
    black_box(float_sink);
    black_box(mem_sink);

    Calibration {
        int_ns_per_op: int_ns,
        float_ns_per_op: float_ns,
        mem_gib_per_sec: mem_gib / mem_seconds,
    }
}
