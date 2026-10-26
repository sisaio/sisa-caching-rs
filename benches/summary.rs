//! Prints the last `cargo bench` results as a Markdown table, with the machine they ran on. Each
//! time is Criterion's mean, with its 95% confidence interval.
//!
//! ```sh
//! cargo bench -p sisa-caching-benches
//! cargo run -p sisa-caching-benches --bin summary
//! ```
//!
//! Given a release, such as `v0.1.0`, it compares that release with this code instead, run on the
//! same machine and saved as the Criterion baselines `previous` and `current`:
//!
//! ```sh
//! git worktree add ../previous v0.1.0
//! (cd ../previous && CARGO_TARGET_DIR="$OLDPWD/target" \
//!     cargo bench -p sisa-caching-benches -- --save-baseline previous)
//! cargo bench -p sisa-caching-benches -- --save-baseline current
//! cargo run -p sisa-caching-benches --bin summary -- v0.1.0
//! ```

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
};

use serde::Deserialize;

/// Two runs of the same code on one runner can differ by a few percent, so a smaller change isn't
/// called one.
const NOISE_PERCENT: f64 = 5.0;

#[derive(Deserialize)]
struct Benchmark {
    full_id: String,
}

#[derive(Deserialize)]
struct Estimates {
    mean: Estimate,
}

#[derive(Deserialize)]
struct Estimate {
    point_estimate: f64,
    confidence_interval: Interval,
}

#[derive(Deserialize)]
struct Interval {
    lower_bound: f64,
    upper_bound: f64,
}

fn main() {
    let target = env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "target".into());
    let previous = env::args().nth(1);
    // Criterion keeps each benchmark's latest run in `new`, and a saved baseline under its name.
    let latest = if previous.is_some() { "current" } else { "new" };
    let mut runs = Vec::new();

    find_runs(&Path::new(&target).join("criterion"), latest, &mut runs);
    runs.sort();

    println!("## Benchmarks\n");
    println!(
        "Ran on {}, against `{}` in Docker on the same machine.\n",
        machine(),
        redis_image()
    );

    match &previous {
        Some(release) => {
            println!(
                "Both columns ran in the same job, so they compare fairly. A change counts only \
                 when the two 95% intervals don't overlap and the means differ by \
                 {NOISE_PERCENT}% or more.\n"
            );
            println!("| Benchmark | {release} | This release | 95% interval | Change |");
            println!("| --- | ---: | ---: | ---: | --- |");
        }
        None => {
            println!("| Benchmark | Time | 95% interval |");
            println!("| --- | ---: | ---: |");
        }
    }

    for run in runs {
        let id: Benchmark = read(&run.join("benchmark.json"));
        let Estimates { mean } = read(&run.join("estimates.json"));
        let interval = format!(
            "{} to {}",
            time(mean.confidence_interval.lower_bound),
            time(mean.confidence_interval.upper_bound)
        );

        if previous.is_none() {
            println!(
                "| {} | {} | {interval} |",
                id.full_id,
                time(mean.point_estimate)
            );
            continue;
        }

        // The baseline sits next to the run; a benchmark the previous release didn't have has none.
        let baseline = run.with_file_name("previous").join("estimates.json");
        let (before, change) = if baseline.exists() {
            let Estimates { mean: before } = read(&baseline);

            (time(before.point_estimate), change(&before, &mean))
        } else {
            (String::new(), "new".to_owned())
        };

        println!(
            "| {} | {before} | {} | {interval} | {change} |",
            id.full_id,
            time(mean.point_estimate)
        );
    }
}

/// How much faster or slower `after` is, or "about the same" when the intervals overlap or the
/// means differ by less than [`NOISE_PERCENT`].
fn change(before: &Estimate, after: &Estimate) -> String {
    let (before_ci, after_ci) = (&before.confidence_interval, &after.confidence_interval);
    let overlap = after_ci.upper_bound >= before_ci.lower_bound
        && after_ci.lower_bound <= before_ci.upper_bound;
    let percent = (after.point_estimate / before.point_estimate - 1.0) * 100.0;

    if overlap || percent.abs() < NOISE_PERCENT {
        return "about the same".to_owned();
    }

    if percent < 0.0 {
        format!("{:.0}% faster", -percent)
    } else {
        format!("{percent:.0}% slower")
    }
}

/// Every run folder named `name` under `dir`.
fn find_runs(dir: &Path, name: &str, runs: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for path in entries.flatten().map(|entry| entry.path()) {
        if path.ends_with(name) && path.join("estimates.json").exists() {
            runs.push(path);
        } else if path.is_dir() {
            find_runs(&path, name, runs);
        }
    }
}

fn read<T: for<'de> Deserialize<'de>>(path: &Path) -> T {
    let json = fs::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));

    serde_json::from_slice(&json).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Nanoseconds, in the unit that reads best.
fn time(ns: f64) -> String {
    match ns {
        ns if ns < 1e3 => format!("{ns:.0} ns"),
        ns if ns < 1e6 => format!("{:.1} µs", ns / 1e3),
        ns => format!("{:.1} ms", ns / 1e6),
    }
}

/// Such as "linux x86_64, 4 CPUs (Intel(R) Xeon(R) Processor @ 2.80GHz), rustc 1.99.0".
fn machine() -> String {
    let cpus = thread::available_parallelism().map_or(1, |n| n.get());
    let model = fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|info| {
            info.lines()
                .find_map(|line| line.strip_prefix("model name")?.split_once(':'))
                .map(|(_, model)| format!(" ({})", model.trim()))
        })
        .unwrap_or_default();
    let rustc = Command::new("rustc")
        .arg("--version")
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .unwrap_or_default();

    format!(
        "{} {}, {cpus} CPUs{model}, {rustc}",
        env::consts::OS,
        env::consts::ARCH
    )
}

/// The server the Redis and hybrid benchmarks ran against, as `sisa-caching-testing` picks it.
fn redis_image() -> String {
    env::var("REDIS_IMAGE").unwrap_or_else(|_| "redis:8-alpine".into())
}
