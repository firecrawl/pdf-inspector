//! Steady-state throughput harness for the Rust reference implementation.
//!
//! Mirrors `src/PdfInspector.Bench` so the two sides can be compared directly:
//! same fixture set, same warmup/measure protocol, same statistics, same
//! machine-speed kernels, same JSON shape.

mod kernels;

use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage();
        return;
    }

    let opts = Options::parse(&args);
    let fixtures = resolve_fixtures(&opts);
    if fixtures.is_empty() {
        eprintln!("No fixtures matched. Looked in {}", opts.fixture_dir.display());
        std::process::exit(1);
    }

    eprintln!("cpu: {}", describe_machine());

    let calibration = kernels::calibrate();
    eprintln!("calibration: {}", calibration.describe());
    eprintln!(
        "harness: warmup>={} iters, measure>={} iters or {} ms per fixture",
        opts.warmup_iterations, opts.min_iterations, opts.budget_ms
    );
    eprintln!();

    let mut results = Vec::with_capacity(fixtures.len());
    for path in &fixtures {
        let result = measure(path, &opts);
        eprintln!("{}", result.to_line());
        results.push(result);
    }

    let total: f64 = results.iter().map(FixtureResult::median).sum();
    eprintln!();
    eprintln!(
        "total: {:.1} ms median across {} fixtures",
        total,
        results.len()
    );

    if let Some(json_path) = &opts.json_path {
        let json = build_json(calibration, &results, &opts);
        std::fs::write(json_path, json).expect("write json");
        eprintln!("wrote {}", json_path.display());
    }
}

fn print_usage() {
    eprintln!("Usage: pdf-inspector-bench [options]");
    eprintln!();
    eprintln!("  --fixtures <dir>   Directory of .pdf fixtures");
    eprintln!("  --filter <substr>  Only fixtures whose name contains this");
    eprintln!("  --iters <n>        Minimum timed iterations per fixture (default 5)");
    eprintln!("  --warmup <n>       Minimum warmup iterations per fixture (default 3)");
    eprintln!("  --budget <ms>      Keep timing a fixture until this budget is spent (default 3000)");
    eprintln!("  --json <path>      Write machine-readable results here");
}

struct Options {
    fixture_dir: PathBuf,
    filter: Option<String>,
    min_iterations: usize,
    warmup_iterations: usize,
    budget_ms: u128,
    json_path: Option<PathBuf>,
}

impl Options {
    fn parse(args: &[String]) -> Self {
        let value = |name: &str| -> Option<String> {
            args.iter()
                .position(|a| a == name)
                .and_then(|i| args.get(i + 1))
                .cloned()
        };
        let num = |name: &str, fallback: usize| -> usize {
            value(name)
                .and_then(|v| v.parse().ok())
                .unwrap_or(fallback)
        };

        Self {
            fixture_dir: value("--fixtures")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("reference/tests/fixtures")),
            filter: value("--filter"),
            min_iterations: num("--iters", 5),
            warmup_iterations: num("--warmup", 3),
            budget_ms: num("--budget", 3000) as u128,
            json_path: value("--json").map(PathBuf::from),
        }
    }
}

fn resolve_fixtures(opts: &Options) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(&opts.fixture_dir) else {
        return Vec::new();
    };

    let mut out: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "pdf"))
        .filter(|p| {
            let name = p.file_name().unwrap_or_default().to_string_lossy();
            // The encrypted fixture needs a password; neither harness supplies one.
            !name.starts_with("encrypted-")
                && opts.filter.as_ref().is_none_or(|f| {
                    name.to_lowercase().contains(&f.to_lowercase())
                })
        })
        .collect();
    out.sort();
    out
}

/// One full extraction — the same call the CLI's `--raw` path makes.
fn run_once(bytes: &[u8]) -> usize {
    match pdf_inspector::process_pdf_mem(bytes) {
        Ok(result) => result.markdown.map_or(0, |m| m.chars().count()),
        Err(_) => 0,
    }
}

fn measure(path: &Path, opts: &Options) -> FixtureResult {
    let name = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let bytes = std::fs::read(path).expect("read fixture");

    // Warmup, under exactly the rule the C# harness uses: repeat until the call
    // target is met or the wall clock runs out. There is no JIT here, so the
    // repetitions buy only settled caches, branch predictors and allocator free
    // lists — but running the identical protocol on both sides is what keeps the
    // comparison honest. See `WarmUntilStable` in the C# harness for why the
    // count is what it is.
    const TIER_UP_CALL_TARGET: usize = 100;
    let max_warmup_ms = (opts.budget_ms * 2).max(10_000);

    let warm_start = Instant::now();
    let mut warmups = 0usize;
    loop {
        black_box(run_once(&bytes));
        warmups += 1;

        if warmups >= opts.warmup_iterations && warmups >= TIER_UP_CALL_TARGET {
            break;
        }
        if warm_start.elapsed().as_millis() > max_warmup_ms {
            break;
        }
    }

    let mut samples: Vec<f64> = Vec::with_capacity(opts.min_iterations * 2);
    let total_start = Instant::now();
    let output = loop {
        let t = Instant::now();
        let out = black_box(run_once(&bytes));
        samples.push(t.elapsed().as_secs_f64() * 1e3);

        if total_start.elapsed().as_millis() > opts.budget_ms
            || samples.len() >= opts.min_iterations
        {
            break out;
        }
    };

    FixtureResult {
        name,
        input_bytes: bytes.len(),
        output_chars: output,
        samples,
        warmups,
    }
}

struct FixtureResult {
    name: String,
    input_bytes: usize,
    output_chars: usize,
    samples: Vec<f64>,
    warmups: usize,
}

impl FixtureResult {
    fn sorted(&self) -> Vec<f64> {
        let mut s = self.samples.clone();
        s.sort_by(f64::total_cmp);
        s
    }

    fn percentile(&self, q: f64) -> f64 {
        let s = self.sorted();
        if s.is_empty() {
            return 0.0;
        }
        let idx = (q * (s.len() - 1) as f64).round() as usize;
        s[idx.min(s.len() - 1)]
    }

    /// The headline figure. The median resists the occasional stall a shared VM
    /// inflicts far better than the mean does.
    fn median(&self) -> f64 {
        self.percentile(0.50)
    }

    fn min(&self) -> f64 {
        self.sorted().first().copied().unwrap_or(0.0)
    }

    fn mean(&self) -> f64 {
        self.samples.iter().sum::<f64>() / self.samples.len() as f64
    }

    fn std_dev(&self) -> f64 {
        if self.samples.len() < 2 {
            return 0.0;
        }
        let mean = self.mean();
        let sum_sq: f64 = self.samples.iter().map(|s| (s - mean) * (s - mean)).sum();
        (sum_sq / (self.samples.len() - 1) as f64).sqrt()
    }

    fn to_line(&self) -> String {
        format!(
            "{:<40} {:>9.2} ms  (min {:>8.2}  p95 {:>8.2}  sd {:>6.2})  n={:<3}",
            self.name,
            self.median(),
            self.min(),
            self.percentile(0.95),
            self.std_dev(),
            self.samples.len()
        )
    }

    fn to_json(&self) -> String {
        format!(
            "{{\"name\":{},\"inputBytes\":{},\"outputChars\":{},\"iterations\":{},\
             \"warmups\":{},\"medianMs\":{:.4},\"minMs\":{:.4},\"meanMs\":{:.4},\
             \"p95Ms\":{:.4},\"stdDevMs\":{:.4}}}",
            json_string(&self.name),
            self.input_bytes,
            self.output_chars,
            self.samples.len(),
            self.warmups,
            self.median(),
            self.min(),
            self.mean(),
            self.percentile(0.95),
            self.std_dev()
        )
    }
}

fn build_json(
    calibration: kernels::Calibration,
    results: &[FixtureResult],
    opts: &Options,
) -> String {
    let fixtures: Vec<String> = results.iter().map(FixtureResult::to_json).collect();
    format!(
        "{{\"impl\":\"rust\",\"runtime\":{},\"cpu\":{},\"cores\":{},\"isa\":{},\
         \"budgetMs\":{},\"calibration\":{},\"fixtures\":[{}]}}",
        json_string(&format!("rustc {}", rustc_version())),
        json_string(&cpu_model()),
        num_cores(),
        json_string(widest_isa()),
        opts.budget_ms,
        calibration.to_json(),
        fixtures.join(",")
    )
}

// ---------------------------------------------------------------------------
// Machine identification
// ---------------------------------------------------------------------------

fn describe_machine() -> String {
    format!(
        "{} | {} logical cores | {} | rustc {} | {}",
        cpu_model(),
        num_cores(),
        widest_isa(),
        rustc_version(),
        std::env::consts::OS
    )
}

fn cpu_model() -> String {
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split_once(':'))
                .map(|(_, v)| v.trim().to_string())
        })
        .unwrap_or_else(|| "unknown CPU".to_string())
}

fn num_cores() -> usize {
    std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
}

/// The widest vector instruction set this binary may use. Reported for parity
/// with the C# side; the reference crate itself does not hand-vectorise.
fn widest_isa() -> &'static str {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx512f") {
            return if is_x86_feature_detected!("avx512bw") {
                "AVX-512F+BW"
            } else {
                "AVX-512F"
            };
        }
        if is_x86_feature_detected!("avx2") {
            return "AVX2";
        }
        if is_x86_feature_detected!("sse4.2") {
            return "SSE4.2";
        }
    }
    "scalar"
}

/// The compiler version, captured at build time by `build.rs` when present and
/// otherwise reported as unknown. Kept simple: the harness only needs it for
/// the record, not for any decision.
fn rustc_version() -> String {
    option_env!("BENCH_RUSTC_VERSION")
        .unwrap_or("unknown")
        .to_string()
}

fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
