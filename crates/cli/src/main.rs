//! `imageopt` — command-line entry point.

mod args;
mod report;
mod run;

use clap::Parser;
use imageopt_core::OutputSink;

use crate::args::Cli;

fn main() {
    let cli = Cli::parse();

    if let Some(jobs) = cli.jobs {
        // Best-effort: ignore if a global pool already exists.
        let _ = rayon::ThreadPoolBuilder::new()
            .num_threads(jobs.max(1))
            .build_global();
    }

    let paths = run::expand_paths(&cli.paths, cli.recursive);
    if paths.is_empty() {
        eprintln!("imageopt: no matching files found");
        std::process::exit(2);
    }

    let sink = if cli.writes_files() {
        OutputSink::InPlace { backup: cli.backup }
    } else {
        OutputSink::DryRun
    };

    let opts = cli.to_options();
    let results = run::run(&paths, &opts, &sink, &cli);

    if cli.json {
        report::print_json(&results);
    } else {
        report::print_table(&results, &cli);
        report::print_summary(&results);
        if cli.dry_run {
            anstream::eprintln!("(dry run — no files were modified)");
        } else if cli.check {
            anstream::eprintln!("(check — no files were modified)");
        }
    }

    std::process::exit(report::exit_code(&results, &cli));
}
