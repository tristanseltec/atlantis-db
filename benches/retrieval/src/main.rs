//! `rbench` — the Atlantis Retrieval Bench.
//!
//! ```text
//! cargo run --release -p atlantis-retrieval-bench -- --scale small --out benches/results
//! ```
//!
//! See `benches/retrieval/README.md` for the workloads and options.

mod adapter;
mod atlantis;
mod dataset;
mod truth;
mod workload;

use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use serde::Serialize;

use adapter::{Adapter, Capabilities};
use dataset::{Dataset, Spec};
use truth::Truth;
use workload::{MixedResult, Results, Settings, WriterResult};

const WORKLOADS: &[&str] = &[
    "load",
    "vector",
    "filtered",
    "graphrag",
    "concurrent",
    "mixed",
    "freshness",
    "reopen",
];

struct Args {
    spec: Spec,
    scale: String,
    settings: Settings,
    overfetch: usize,
    ef_search: Option<usize>,
    dir: Option<PathBuf>,
    out: Option<PathBuf>,
    workloads: Vec<String>,
}

fn usage() -> String {
    format!(
        "usage: rbench [--scale tiny|small|medium|full] [--chunks N] [--dim D] [--users U]\n\
         \x20             [--queries Q] [--k K] [--overfetch F] [--ef-search E] [--threads T] [--duration SECS]\n\
         \x20             [--write-rate W] [--probes P] [--dir PATH] [--out DIR]\n\
         \x20             [--workloads {}]",
        WORKLOADS.join(",")
    )
}

fn parse_args() -> anyhow::Result<Args> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut scale = "small".to_string();
    // The scale is read first so explicit sizes override the preset.
    if let Some(i) = raw.iter().position(|a| a == "--scale") {
        scale = raw.get(i + 1).context("--scale needs a value")?.clone();
    }
    let mut spec =
        Spec::preset(&scale).with_context(|| format!("unknown scale {scale}\n{}", usage()))?;
    let mut settings = Settings {
        k: 10,
        threads: 8,
        duration: Duration::from_secs(10),
        write_rate: 2_000,
        freshness_probes: 20,
    };
    let (mut overfetch, mut ef_search, mut dir, mut out) = (10, None, None, None);
    let mut workloads: Vec<String> = WORKLOADS.iter().map(|s| s.to_string()).collect();
    let mut it = raw.iter();
    while let Some(flag) = it.next() {
        let mut value = || it.next().with_context(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--scale" => {
                value()?;
            }
            "--chunks" => spec.chunks = value()?.parse()?,
            "--dim" => spec.dim = value()?.parse()?,
            "--users" => spec.users = value()?.parse()?,
            "--queries" => spec.queries = value()?.parse()?,
            "--k" => settings.k = value()?.parse()?,
            "--overfetch" => overfetch = value()?.parse()?,
            "--ef-search" => ef_search = Some(value()?.parse()?),
            "--threads" => settings.threads = value()?.parse()?,
            "--duration" => settings.duration = Duration::from_secs_f64(value()?.parse()?),
            "--write-rate" => settings.write_rate = value()?.parse()?,
            "--probes" => settings.freshness_probes = value()?.parse()?,
            "--dir" => dir = Some(PathBuf::from(value()?)),
            "--out" => out = Some(PathBuf::from(value()?)),
            "--workloads" => {
                workloads = value()?.split(',').map(|s| s.trim().to_string()).collect();
                if let Some(w) = workloads.iter().find(|w| !WORKLOADS.contains(&w.as_str())) {
                    bail!("unknown workload {w}\n{}", usage());
                }
            }
            "-h" | "--help" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            other => bail!("unknown option {other}\n{}", usage()),
        }
    }
    if spec.users == 0 || spec.chunks == 0 || spec.dim == 0 || spec.queries == 0 {
        bail!("--chunks, --dim, --users and --queries must be positive");
    }
    Ok(Args {
        spec,
        scale,
        settings,
        overfetch,
        ef_search,
        dir,
        out,
        workloads,
    })
}

#[derive(Serialize)]
struct Report<'a> {
    bench: &'static str,
    version: u32,
    date: String,
    git_commit: Option<String>,
    machine: Machine,
    scale: &'a str,
    spec: &'a Spec,
    edges: usize,
    k: usize,
    threads: usize,
    duration_s: f64,
    adapter: String,
    capabilities: Capabilities,
    adapter_notes: String,
    results: &'a Results,
}

#[derive(Serialize)]
struct Machine {
    os: &'static str,
    arch: &'static str,
    cpus: usize,
}

fn main() -> anyhow::Result<()> {
    let args = parse_args()?;
    let has = |w: &str| args.workloads.iter().any(|x| x == w);
    let s = &args.settings;

    eprintln!(
        "generating corpus: {} chunks × {}d, {} users, {} queries",
        args.spec.chunks, args.spec.dim, args.spec.users, args.spec.queries
    );
    let t = Instant::now();
    let ds = Dataset::generate(args.spec.clone());
    eprintln!(
        "  {:.1}s, {} edges",
        t.elapsed().as_secs_f64(),
        ds.edge_count()
    );

    let t = Instant::now();
    let truth = Truth::compute(
        &ds,
        s.k,
        std::thread::available_parallelism().map_or(4, |n| n.get()),
    );
    eprintln!("exact ground truth: {:.1}s", t.elapsed().as_secs_f64());

    if let Some(dir) = &args.dir {
        if dir.exists() && std::fs::read_dir(dir)?.next().is_some() {
            bail!("--dir {} must be empty or absent", dir.display());
        }
        std::fs::create_dir_all(dir)?;
    }
    let adapter = atlantis::Atlantis::open(
        args.dir.clone(),
        args.spec.dim,
        args.overfetch,
        args.ef_search,
    )?;
    let a: &dyn Adapter = &adapter;
    let next_id = AtomicU64::new(args.spec.chunks as u64 + 1_000_000_000);
    let mut r = Results::default();

    // Every other workload needs the data, so `load` always runs.
    eprintln!("[load]");
    r.load = Some(workload::load(a, &ds)?);
    if has("vector") {
        eprintln!("[vector]");
        r.vector = Some(workload::search(a, &ds, &truth, s.k, false)?);
    }
    if has("filtered") {
        eprintln!("[filtered]");
        r.filtered_vector = Some(workload::search(a, &ds, &truth, s.k, true)?);
    }
    if has("graphrag") {
        eprintln!("[graphrag]");
        r.graph_rag = Some(workload::graph_rag(a, &ds, s.k)?);
    }
    if has("concurrent") {
        eprintln!("[concurrent] {} threads, {:?}", s.threads, s.duration);
        r.graph_rag_concurrent = Some(workload::concurrent(a, &ds, s, 0, &next_id)?.0);
    }
    if has("mixed") {
        eprintln!(
            "[mixed] {} threads + {} writes/s, {:?}",
            s.threads, s.write_rate, s.duration
        );
        let (readers, writer) = workload::concurrent(a, &ds, s, s.write_rate, &next_id)?;
        let w: WriterResult = writer.unwrap_or_default();
        r.mixed = Some(MixedResult {
            readers,
            target_writes_per_s: s.write_rate,
            achieved_writes_per_s: w.per_s,
            write_latency: w.latency,
            write_errors: w.errors,
        });
    }
    if has("freshness") {
        eprintln!("[freshness]");
        r.reads_own_writes = Some(workload::reads_own_writes(
            a,
            &ds,
            s.freshness_probes,
            &next_id,
        )?);
    }
    if has("reopen") {
        r.reopen_s = a.reopen()?.map(|d| d.as_secs_f64());
    }

    let report = Report {
        bench: "atlantis-retrieval-bench",
        version: 1,
        date: utc_date(),
        git_commit: git_commit(),
        machine: Machine {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            cpus: std::thread::available_parallelism().map_or(0, |n| n.get()),
        },
        scale: &args.scale,
        spec: &args.spec,
        edges: ds.edge_count(),
        k: s.k,
        threads: s.threads,
        duration_s: s.duration.as_secs_f64(),
        adapter: a.name(),
        capabilities: a.capabilities(),
        adapter_notes: format!(
            "filtered search = vector top-{}×{} then WHERE; efSearch {}; no BM25 (graph-RAG is vector-only)",
            s.k,
            args.overfetch,
            args.ef_search.map_or("default".to_string(), |e| e.to_string())
        ),
        results: &r,
    };
    let markdown = markdown(&report);
    println!("{markdown}");
    if let Some(out) = &args.out {
        std::fs::create_dir_all(out)?;
        let ef = args.ef_search.map_or(String::new(), |e| format!("-ef{e}"));
        let stem = format!(
            "{}-{}-{}{ef}",
            report.date,
            args.scale,
            if args.dir.is_some() { "dir" } else { "mem" }
        );
        std::fs::write(
            out.join(format!("{stem}.json")),
            serde_json::to_string_pretty(&report)?,
        )?;
        std::fs::write(out.join(format!("{stem}.md")), &markdown)?;
        eprintln!("wrote {}/{stem}.{{json,md}}", out.display());
    }
    Ok(())
}

fn markdown(r: &Report) -> String {
    use std::fmt::Write;
    let mut m = String::new();
    let res = r.results;
    let _ = writeln!(m, "## Retrieval Bench — {} — {}\n", r.adapter, r.scale);
    let _ = writeln!(
        m,
        "{} chunks × {}d, {} users (filter selectivity {:.2}%), {} edges, {} queries, k = {}. \
         {} {} with {} CPUs; commit {}; {}.\n",
        r.spec.chunks,
        r.spec.dim,
        r.spec.users,
        100.0 / r.spec.users as f64,
        r.edges,
        r.spec.queries,
        r.k,
        r.machine.os,
        r.machine.arch,
        r.machine.cpus,
        r.git_commit.as_deref().unwrap_or("unknown"),
        r.date
    );
    let c = &r.capabilities;
    let _ = writeln!(
        m,
        "Capabilities: filter in search {}, BM25 hybrid {}, search reads own writes {}, graph in query {}, embedded {}. Notes: {}.\n",
        yn(c.filter_in_search),
        yn(c.hybrid_text),
        yn(c.search_reads_own_writes),
        yn(c.graph_in_query),
        yn(c.embedded),
        r.adapter_notes
    );
    let _ = writeln!(m, "| Workload | p50 ms | p99 ms | Mean ms | Other |");
    let _ = writeln!(m, "|---|---|---|---|---|");
    if let Some(l) = &res.load {
        let _ = writeln!(
            m,
            "| Load | | | | {} items in {:.1}s ({:.0}/s); index build {:.2}s |",
            l.items, l.load_s, l.items_per_s, l.index_build_s
        );
    }
    let row = |m: &mut String, name: &str, l: &workload::Latency, other: String| {
        let _ = writeln!(
            m,
            "| {name} | {:.3} | {:.3} | {:.3} | {other} |",
            l.p50_ms, l.p99_ms, l.mean_ms
        );
    };
    if let Some(v) = &res.vector {
        row(
            &mut m,
            "Vector top-k",
            &v.latency,
            format!("recall@k {:.3}", v.recall_at_k),
        );
    }
    if let Some(v) = &res.filtered_vector {
        row(
            &mut m,
            "Filtered vector top-k",
            &v.latency,
            format!(
                "recall@k {:.3}; {} of {} queries short",
                v.recall_at_k, v.short_results, v.latency.count
            ),
        );
    }
    if let Some(l) = &res.graph_rag {
        row(&mut m, "Graph-RAG, 1 thread", l, String::new());
    }
    if let Some(cr) = &res.graph_rag_concurrent {
        row(
            &mut m,
            &format!("Graph-RAG, {} threads", cr.threads),
            &cr.latency,
            format!("{:.0} QPS; {} errors", cr.qps, cr.errors),
        );
    }
    if let Some(mx) = &res.mixed {
        row(
            &mut m,
            &format!("Graph-RAG, {} threads + writes", mx.readers.threads),
            &mx.readers.latency,
            format!(
                "{:.0} QPS; writes {:.0}/s of {} target",
                mx.readers.qps, mx.achieved_writes_per_s, mx.target_writes_per_s
            ),
        );
        row(
            &mut m,
            "Write (mixed)",
            &mx.write_latency,
            format!("{} errors", mx.write_errors),
        );
    }
    if let Some(f) = res.reads_own_writes {
        let _ = writeln!(
            m,
            "| Search reads own write | | | | {:.0}% of probes |",
            f * 100.0
        );
    }
    if let Some(s) = res.reopen_s {
        let _ = writeln!(m, "| Reopen to first answer | | | | {s:.2}s |");
    }
    m
}

fn yn(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

fn git_commit() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `YYYY-MM-DD` in UTC, without a date dependency.
fn utc_date() -> String {
    let days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 86_400) as i64;
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}
