#![allow(dead_code)]

mod context;
mod format;
mod openclaw;
mod parser;
mod session;
mod transform;
mod types;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use clap::Parser;

use format::FormatOptions;

#[derive(Parser)]
#[command(name = "agenticlaw", about = "Native .ctx agent context files — reads .ctx and .jsonl")]
struct Cli {
    /// Path to a .ctx/.jsonl file or directory
    path: Option<String>,

    /// Output directory. If omitted, prints to stdout.
    #[arg(short, long)]
    output: Option<String>,

    /// Emit clean context (.ctx) format instead of human-readable display
    #[arg(long, default_value_t = false)]
    ctx: bool,

    /// Emit JSONL wire format (for LLM API transport)
    #[arg(long, default_value_t = false)]
    wire: bool,

    /// Include [thinking] blocks in output
    #[arg(long, default_value_t = false)]
    include_thinking: bool,

    /// Show token usage per assistant turn
    #[arg(long, default_value_t = false)]
    include_usage: bool,

    /// Print session summary only, no conversation
    #[arg(long, default_value_t = false)]
    summary: bool,

    /// Don't truncate long tool results
    #[arg(long, default_value_t = false)]
    raw: bool,

    /// Print version and supported openclaw format version
    #[arg(long)]
    version: bool,

    /// Watch directory for new/changed files and transform on arrival
    #[arg(long, default_value_t = false)]
    watch: bool,
}

fn main() {
    let cli = Cli::parse();

    if cli.version {
        println!(
            "agenticlaw {} (openclaw format {})",
            env!("CARGO_PKG_VERSION"),
            openclaw::MAX_SUPPORTED_VERSION
        );
        return;
    }

    let opts = FormatOptions {
        include_thinking: cli.include_thinking,
        include_usage: cli.include_usage,
        summary_only: cli.summary,
        raw: cli.raw,
    };

    let Some(ref input_path) = cli.path else {
        eprintln!("Error: <PATH> argument is required");
        std::process::exit(1);
    };
    let path = Path::new(input_path);

    if cli.watch {
        let Some(ref out_dir) = cli.output else {
            eprintln!("Error: --watch requires -o <output dir>");
            std::process::exit(1);
        };
        if !path.is_dir() {
            eprintln!("Error: --watch requires a directory path");
            std::process::exit(1);
        }
        let emit_opts = context::EmitOptions {
            include_thinking: cli.include_thinking,
            include_usage: cli.include_usage,
            raw: cli.raw,
        };
        watch_loop(path, Path::new(out_dir), &opts, &emit_opts, cli.ctx, cli.wire);
        return;
    }

    let files = collect_session_files(path);
    if files.is_empty() {
        eprintln!("No .ctx or .jsonl files found at: {}", input_path);
        return;
    }

    let emit_opts = context::EmitOptions {
        include_thinking: cli.include_thinking,
        include_usage: cli.include_usage,
        raw: cli.raw,
    };

    for file in &files {
        match process_file(file, &opts, &emit_opts, cli.ctx, cli.wire) {
            Ok(output_text) => {
                if let Some(ref out_dir) = cli.output {
                    let ext = if cli.ctx { "ctx" } else if cli.wire { "jsonl" } else { "txt" };
                    write_output(file, &output_text, Path::new(out_dir), ext);
                } else {
                    print!("{}", output_text);
                    if files.len() > 1 {
                        println!("\n");
                    }
                }
            }
            Err(e) => {
                eprintln!("Error processing {}: {}", file.display(), e);
            }
        }
    }
}

fn write_output(file: &Path, content: &str, out_path: &Path, ext: &str) {
    fs::create_dir_all(out_path).expect("Failed to create output directory");
    let stem = file.file_stem().unwrap().to_string_lossy();
    let out_file = out_path.join(format!("{}.{}", stem, ext));
    fs::write(&out_file, content).expect("Failed to write output file");
    eprintln!("Wrote: {}", out_file.display());
}

fn watch_loop(dir: &Path, out_dir: &Path, format_opts: &FormatOptions, emit_opts: &context::EmitOptions, output_ctx: bool, output_wire: bool) {
    let mut seen: HashMap<PathBuf, SystemTime> = HashMap::new();
    let ext = if output_ctx { "ctx" } else if output_wire { "jsonl" } else { "txt" };
    eprintln!("Watching {} for changes (Ctrl-C to stop)...", dir.display());

    // Initial pass
    for file in collect_session_files(dir) {
        if let Ok(mtime) = fs::metadata(&file).and_then(|m| m.modified()) {
            match process_file(&file, format_opts, emit_opts, output_ctx, output_wire) {
                Ok(text) => {
                    write_output(&file, &text, out_dir, ext);
                    seen.insert(file, mtime);
                }
                Err(e) => eprintln!("Error processing {}: {}", file.display(), e),
            }
        }
    }

    loop {
        std::thread::sleep(Duration::from_secs(1));
        for file in collect_session_files(dir) {
            let mtime = match fs::metadata(&file).and_then(|m| m.modified()) {
                Ok(t) => t,
                Err(_) => continue,
            };
            if seen.get(&file) == Some(&mtime) {
                continue;
            }
            match process_file(&file, format_opts, emit_opts, output_ctx, output_wire) {
                Ok(text) => {
                    write_output(&file, &text, out_dir, ext);
                    seen.insert(file, mtime);
                }
                Err(e) => eprintln!("Error processing {}: {}", file.display(), e),
            }
        }
    }
}

fn is_session_file(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "jsonl" || e == "ctx")
}

fn collect_session_files(path: &Path) -> Vec<std::path::PathBuf> {
    if path.is_file() && is_session_file(path) {
        return vec![path.to_path_buf()];
    }

    if path.is_dir() {
        let mut files: Vec<_> = fs::read_dir(path)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| is_session_file(p))
            .collect();
        files.sort();
        return files;
    }

    vec![]
}

fn process_file(
    path: &Path,
    format_opts: &FormatOptions,
    emit_opts: &context::EmitOptions,
    output_ctx: bool,
    output_wire: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    use session::Session;

    let is_ctx = path.extension().is_some_and(|e| e == "ctx");

    if is_ctx {
        let session = context::CleanContextSession::from_file(path)?;
        for err in &session.parse_errors {
            eprintln!("  Warning: {}:{}: {}", path.display(), err.line, err.message);
        }
        if output_wire {
            Ok(context::to_wire(session.events()))
        } else if output_ctx {
            // Already .ctx — re-emit (normalizes formatting)
            Ok(context::emit(session.events(), emit_opts))
        } else {
            Ok(format::format_session(session.events(), format_opts))
        }
    } else {
        let session = openclaw::OpenclawSession::from_jsonl(path)?;
        for err in &session.parse_errors {
            eprintln!("  Warning: {}:{}: {}", path.display(), err.line, err.message);
        }
        if output_ctx {
            Ok(context::emit(session.events(), emit_opts))
        } else if output_wire {
            Ok(context::to_wire(session.events()))
        } else {
            Ok(format::format_session(session.events(), format_opts))
        }
    }
}

#[cfg(test)]
mod tests;
