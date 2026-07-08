//! Human-readable table, summary, and JSON output.

use std::collections::BTreeMap;

use imageopt_core::{OptimizeResult, OptimizeStatus};
use owo_colors::OwoColorize;

use crate::args::Cli;

/// Format a byte count like `1.8 MB`.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.1} {}", UNITS[unit])
}

fn name_of(r: &OptimizeResult) -> String {
    r.source
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<bytes>".to_string())
}

fn colored_status(status: &OptimizeStatus) -> String {
    let text = status.label();
    match status {
        OptimizeStatus::Optimized => text.green().to_string(),
        OptimizeStatus::AlreadyOptimal => text.dimmed().to_string(),
        OptimizeStatus::Skipped { .. } => text.yellow().to_string(),
        OptimizeStatus::Failed { .. } => text.red().to_string(),
    }
}

/// Print the per-file results table (skipped entirely in quiet mode).
pub fn print_table(results: &[OptimizeResult], cli: &Cli) {
    if cli.quiet || results.is_empty() {
        return;
    }

    let name_w = results
        .iter()
        .map(|r| name_of(r).chars().count())
        .max()
        .unwrap_or(4)
        .clamp(4, 60);

    anstream::println!(
        "{:<name_w$}  {:<6}  {:>10}  {:>10}  {:>8}  {}",
        "FILE".bold(),
        "FORMAT".bold(),
        "ORIGINAL".bold(),
        "NEW".bold(),
        "SAVED".bold(),
        "STATUS".bold(),
    );

    for r in results {
        let (saved, new_size) = if r.is_optimized() {
            (
                format!("{:.1}%", r.saved_percent()),
                human_size(r.optimized_size),
            )
        } else {
            ("—".to_string(), "—".to_string())
        };
        let detail = match &r.status {
            OptimizeStatus::Failed { error } => format!(" ({error})"),
            OptimizeStatus::Skipped { reason } => format!(" ({reason})"),
            _ => String::new(),
        };
        anstream::println!(
            "{:<name_w$}  {:<6}  {:>10}  {:>10}  {:>8}  {}{}",
            truncate(&name_of(r), name_w),
            r.format.as_str(),
            human_size(r.original_size),
            new_size,
            saved,
            colored_status(&r.status),
            detail.dimmed(),
        );
    }
}

/// Print the aggregate summary line.
pub fn print_summary(results: &[OptimizeResult]) {
    let summary = summarize(results);
    let pct = summary.saved_percent;

    let mut parts = vec![format!("{} optimized", summary.optimized)];
    if summary.already_optimal > 0 {
        parts.push(format!("{} already-optimal", summary.already_optimal));
    }
    if summary.skipped > 0 {
        parts.push(format!("{} skipped", summary.skipped));
    }
    if summary.failed > 0 {
        parts.push(format!("{} failed", summary.failed));
    }

    let change = if summary.saved_bytes >= 0 {
        format!(
            "-{pct:.1}%, saved {}",
            human_size(summary.saved_bytes as u64)
        )
        .green()
        .to_string()
    } else {
        format!(
            "+{:.1}%, grew {}",
            -pct,
            human_size((-summary.saved_bytes) as u64)
        )
        .red()
        .to_string()
    };

    anstream::println!(
        "\n{}  {}  {} → {}  ({})",
        "TOTAL".bold(),
        parts.join(", "),
        human_size(summary.original_size),
        human_size(summary.optimized_size),
        change,
    );
}

#[derive(Clone, Debug)]
struct Summary {
    total: u64,
    optimized: u64,
    already_optimal: u64,
    skipped: u64,
    failed: u64,
    original_size: u64,
    optimized_size: u64,
    saved_bytes: i64,
    saved_percent: f64,
    elapsed_ms: u64,
    formats: BTreeMap<String, u64>,
}

fn summarize(results: &[OptimizeResult]) -> Summary {
    let mut summary = Summary {
        total: results.len() as u64,
        optimized: 0,
        already_optimal: 0,
        skipped: 0,
        failed: 0,
        original_size: 0,
        optimized_size: 0,
        saved_bytes: 0,
        saved_percent: 0.0,
        elapsed_ms: 0,
        formats: BTreeMap::new(),
    };

    for r in results {
        summary.original_size += r.original_size;
        summary.optimized_size += match r.status {
            OptimizeStatus::Optimized => r.optimized_size,
            _ => r.original_size,
        };
        summary.elapsed_ms += r.elapsed.as_millis() as u64;
        *summary
            .formats
            .entry(r.format.as_str().to_string())
            .or_default() += 1;

        match r.status {
            OptimizeStatus::Optimized => summary.optimized += 1,
            OptimizeStatus::AlreadyOptimal => summary.already_optimal += 1,
            OptimizeStatus::Skipped { .. } => summary.skipped += 1,
            OptimizeStatus::Failed { .. } => summary.failed += 1,
        }
    }

    // Signed so `--keep-larger` (which can grow the total) is reported honestly.
    summary.saved_bytes = summary.original_size as i64 - summary.optimized_size as i64;
    summary.saved_percent = if summary.original_size > 0 {
        summary.saved_bytes as f64 / summary.original_size as f64 * 100.0
    } else {
        0.0
    };
    summary
}

/// Print results as a JSON array (one object per file).
pub fn print_json(results: &[OptimizeResult]) {
    let items: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            let (status, error) = match &r.status {
                OptimizeStatus::Optimized => ("optimized", None),
                OptimizeStatus::AlreadyOptimal => ("already_optimal", None),
                OptimizeStatus::Skipped { reason } => ("skipped", Some(reason.clone())),
                OptimizeStatus::Failed { error } => ("failed", Some(error.clone())),
            };
            serde_json::json!({
                "file": r.source.as_ref().map(|p| p.display().to_string()),
                "format": r.format.as_str(),
                "status": status,
                "error": error,
                "original_size": r.original_size,
                "optimized_size": if r.is_optimized() {
                    r.optimized_size
                } else {
                    r.original_size
                },
                "saved_bytes": if r.is_optimized() { r.saved_bytes() } else { 0 },
                "saved_percent": if r.is_optimized() {
                    (r.saved_percent() * 100.0).round() / 100.0
                } else {
                    0.0
                },
                "elapsed_ms": r.elapsed.as_millis() as u64,
            })
        })
        .collect();

    let summary = summarize(results);
    let out = serde_json::json!({
        "summary": {
            "total": summary.total,
            "optimized": summary.optimized,
            "already_optimal": summary.already_optimal,
            "skipped": summary.skipped,
            "failed": summary.failed,
            "original_size": summary.original_size,
            "optimized_size": summary.optimized_size,
            "saved_bytes": summary.saved_bytes,
            "saved_percent": (summary.saved_percent * 100.0).round() / 100.0,
            "elapsed_ms": summary.elapsed_ms,
            "formats": summary.formats,
        },
        "results": items,
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
}

/// Compute the process exit code.
///
/// * `--check`: 1 if any file could be optimized or failed, else 0.
/// * otherwise: 0 (failures are reported but do not fail an automated run).
pub fn exit_code(results: &[OptimizeResult], cli: &Cli) -> i32 {
    if cli.check {
        let any = results.iter().any(|r| {
            matches!(
                r.status,
                OptimizeStatus::Optimized | OptimizeStatus::Failed { .. }
            )
        });
        if any {
            return 1;
        }
    }
    0
}

fn truncate(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    let tail: String = s.chars().skip(count - keep).collect();
    format!("…{tail}")
}
