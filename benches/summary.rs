//! Prints the last `cargo bench` results as a Markdown table, with the machine they ran on. Each
//! time is Criterion's mean, with its 95% confidence interval.
//!
//! ```sh
//! cargo bench -p sisa-caching-benches
//! cargo run -p sisa-caching-benches --bin summary
//! ```
//!
//! It lists every result under `target/criterion`, or `$CRITERION_HOME` when that is set, so
//! remove that folder first where benchmarks ran before: a benchmark renamed since then still
//! shows there with its old result.
//!
//! Given a release, such as `v0.1.0`, it compares that release with this code instead. The two
//! run in turns on the same machine, a round each, saved as the Criterion baselines `previous-1`,
//! `current-1`, `previous-2` and so on, so that a machine that drifts during the job slows both
//! alike. docs/05-benchmarks.md has the commands.

use std::{
    env, fs,
    num::NonZero,
    path::{Path, PathBuf},
    process::Command,
    thread,
};

use serde::Deserialize;

/// Two runs of the same code on one runner can differ by a few percent, so a smaller change isn't
/// called one.
const NOISE_PERCENT: f64 = 5.0;

/// A benchmark's mean in each round, in nanoseconds.
struct Rounds(Vec<f64>);

impl Rounds {
    /// The means saved as `<name>-1`, `<name>-2` and so on beside `run`, until one is missing.
    fn saved(run: &Path, name: &str) -> Self {
        let means = (1..)
            .map(|round| {
                run.with_file_name(format!("{name}-{round}"))
                    .join("estimates.json")
            })
            .take_while(|path| path.exists())
            .map(|path| read::<Estimates>(&path).mean.point_estimate)
            .collect();

        Self(means)
    }

    fn median(&self) -> f64 {
        let mut means = self.0.clone();

        means.sort_by(f64::total_cmp);
        means[means.len() / 2]
    }

    fn min(&self) -> f64 {
        self.0.iter().copied().fold(f64::INFINITY, f64::min)
    }

    fn max(&self) -> f64 {
        self.0.iter().copied().fold(f64::NEG_INFINITY, f64::max)
    }
}

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

/// How much faster or slower `after` is, or "about the same" when a round of one overlaps the
/// rounds of the other or the medians differ by less than [`NOISE_PERCENT`]. Criterion's interval
/// covers the noise inside one run only, so the rounds stand for the machine's drift between runs.
fn change(before: &Rounds, after: &Rounds) -> String {
    let apart = after.min() > before.max() || after.max() < before.min();
    let percent = (after.median() / before.median() - 1.0) * 100.0;

    if !apart || percent.abs() < NOISE_PERCENT {
        return "about the same".to_owned();
    }

    if percent < 0.0 {
        format!("{:.0}% faster", -percent)
    } else {
        format!("{percent:.0}% slower")
    }
}

fn main() {
    let home = env::var("CRITERION_HOME").map_or_else(
        |_| {
            let target = env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "target".into());

            Path::new(&target).join("criterion")
        },
        PathBuf::from,
    );
    let previous = env::args().nth(1);
    // Criterion keeps each benchmark's latest run in `new`, and a saved baseline under its name.
    let latest = if previous.is_some() {
        "current-1"
    } else {
        "new"
    };
    let mut runs = Vec::new();

    find_runs(&home, latest, &mut runs);
    runs.sort();

    println!("## Benchmarks\n");
    println!(
        "Ran on {}, against `{}` in Docker on the same machine.\n",
        machine(),
        redis_image()
    );

    let Some(release) = &previous else {
        println!("| Benchmark | Time | 95% interval |");
        println!("| --- | ---: | ---: |");

        for run in runs {
            let id: Benchmark = read(&run.join("benchmark.json"));
            let Estimates { mean } = read(&run.join("estimates.json"));

            println!(
                "| {} | {} | {} to {} |",
                id.full_id,
                time(mean.point_estimate),
                time(mean.confidence_interval.lower_bound),
                time(mean.confidence_interval.upper_bound)
            );
        }
        return;
    };

    println!(
        "Both columns ran in the same job, in alternating rounds: {release}, then this release, \
         and again. Each time is the median of the rounds' means, and the range is this \
         release's lowest to highest round. A change counts only when no round of one overlaps the \
         rounds of the other and the medians differ by {NOISE_PERCENT}% or more.\n"
    );
    println!("| Benchmark | {release} | This release | Range over rounds | Change |");
    println!("| --- | ---: | ---: | ---: | --- |");

    for run in runs {
        let id: Benchmark = read(&run.join("benchmark.json"));
        let after = Rounds::saved(&run, "current");
        // A benchmark the previous release didn't have has no rounds of its own.
        let before = Rounds::saved(&run, "previous");
        let (before_time, change) = if before.0.is_empty() {
            (String::new(), "new".to_owned())
        } else {
            (time(before.median()), change(&before, &after))
        };

        println!(
            "| {} | {before_time} | {} | {} to {} | {change} |",
            id.full_id,
            time(after.median()),
            time(after.min()),
            time(after.max())
        );
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
    let cpus = thread::available_parallelism().map_or(1, NonZero::get);
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
