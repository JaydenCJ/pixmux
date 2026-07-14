//! pixmux CLI entry point.

use std::io::{Read, Write};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use pixmux::doctor::EnvSnapshot;
use pixmux::emit;
use pixmux::transform::{Target, Transformer};

#[derive(Parser)]
#[command(
    name = "pixmux",
    version,
    about = "Kitty graphics passthrough shim for tmux and zellij",
    long_about = "pixmux lets kitty graphics protocol images survive tmux and zellij:\n\
                  it wraps them in tmux passthrough sequences or transcodes them to\n\
                  sixel for zellij, streaming and without patching your multiplexer."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a command in a PTY, translating its kitty graphics output
    Run {
        /// Output target (auto = detect from $ZELLIJ / $TMUX)
        #[arg(long, default_value = "auto")]
        target: Target,
        /// Print translation statistics to stderr on exit
        #[arg(long, short)]
        verbose: bool,
        /// The command to run and its arguments
        #[arg(required = true, trailing_var_arg = true)]
        command: Vec<String>,
    },
    /// Translate a byte stream: stdin in, translated stdout out
    Filter {
        /// Output target (auto = detect from $ZELLIJ / $TMUX)
        #[arg(long, default_value = "auto")]
        target: Target,
        /// Print translation statistics to stderr on exit
        #[arg(long, short)]
        verbose: bool,
    },
    /// Display a PNG image via the kitty graphics protocol
    Cat {
        /// Path to a PNG file
        path: String,
        /// Output target (auto = detect from $ZELLIJ / $TMUX)
        #[arg(long, default_value = "auto")]
        target: Target,
        /// Assign an explicit kitty image id
        #[arg(long)]
        id: Option<u32>,
    },
    /// Diagnose multiplexer / terminal graphics support
    Doctor,
}

fn cmd_filter(target: Target, verbose: bool) -> Result<(), Box<dyn std::error::Error>> {
    let mut transformer = Transformer::new(target.resolve());
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    let mut buf = [0u8; 65536];
    loop {
        let n = stdin.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let out = transformer.feed(&buf[..n]);
        stdout.write_all(&out)?;
        stdout.flush()?;
    }
    stdout.write_all(&transformer.finish())?;
    stdout.flush()?;
    if verbose {
        let st = transformer.stats();
        eprintln!(
            "pixmux: {} graphics command(s), {} translated, {} untranslated, {} image(s) decoded",
            st.graphics_commands, st.translated, st.untranslated, st.images_decoded
        );
        for note in transformer.notes() {
            eprintln!("pixmux: note: {note}");
        }
    }
    Ok(())
}

fn cmd_cat(path: &str, target: Target, id: Option<u32>) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    if !emit::looks_like_png(&bytes) {
        return Err(format!("{path} is not a PNG file (only PNG is supported)").into());
    }
    let kitty = emit::png_to_kitty(&bytes, id);
    let mut transformer = Transformer::new(target.resolve());
    let mut out = transformer.feed(&kitty);
    out.extend(transformer.finish());
    if transformer.stats().untranslated > 0 {
        for note in transformer.notes() {
            eprintln!("pixmux: note: {note}");
        }
    }
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&out)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

fn cmd_doctor() {
    let snap = EnvSnapshot::from_env();
    println!("pixmux doctor");
    println!("-------------");
    for f in snap.findings() {
        println!("{:<18} {}", f.label, f.value);
        if let Some(hint) = f.hint {
            println!("{:<18} hint: {}", "", hint);
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result: Result<i32, Box<dyn std::error::Error>> = match cli.command {
        Command::Run {
            target,
            verbose,
            command,
        } => pixmux::pty::run(&command, target.resolve(), verbose),
        Command::Filter { target, verbose } => cmd_filter(target, verbose).map(|_| 0),
        Command::Cat { path, target, id } => cmd_cat(&path, target, id).map(|_| 0),
        Command::Doctor => {
            cmd_doctor();
            Ok(0)
        }
    };
    match result {
        Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
        Err(e) => {
            eprintln!("pixmux: error: {e}");
            ExitCode::FAILURE
        }
    }
}
