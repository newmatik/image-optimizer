//! Human-readable table, summary, and JSON output.

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
    let mut optimized = 0u64;
    let mut already = 0u64;
    let mut skipped = 0u64;
    let mut failed = 0u64;
    let mut total_orig = 0u64;
    let mut total_new = 0u64;

    for r in results {
        total_orig += r.original_size;
        total_new += match r.status {
            OptimizeStatus::Optimized => r.optimized_size,
            _ => r.original_size,
        };
        match r.status {
            OptimizeStatus::Optimized => optimized += 1,
            OptimizeStatus::AlreadyOptimal => already += 1,
            OptimizeStatus::Skipped { .. } => skipped += 1,
            OptimizeStatus::Failed { .. } => failed += 1,
        }
    }

    // Signed so `--keep-larger` (which can grow the total) is reported honestly.
    let delta = total_orig as i64 - total_new as i64; // >0 saved, <0 grew
    let pct = if total_orig > 0 {
        delta as f64 / total_orig as f64 * 100.0
    } else {
        0.0
    };

    let mut parts = vec![format!("{optimized} optimized")];
    if already > 0 {
        parts.push(format!("{already} already-optimal"));
    }
    if skipped > 0 {
        parts.push(format!("{skipped} skipped"));
    }
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }

    let change = if delta >= 0 {
        format!("-{pct:.1}%, saved {}", human_size(delta as u64))
            .green()
            .to_string()
    } else {
        format!("+{:.1}%, grew {}", -pct, human_size((-delta) as u64))
            .red()
            .to_string()
    };

    anstream::println!(
        "\n{}  {}  {} → {}  ({})",
        "TOTAL".bold(),
        parts.join(", "),
        human_size(total_orig),
        human_size(total_new),
        change,
    );
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

    let out = serde_json::json!({ "results": items });
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
