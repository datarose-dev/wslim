//! Windows-only entry point. Commands are fully usable without the wizard.

mod archive;
mod backup;
mod cleanup;
mod cli;
mod config;
mod interactive;
mod output;
mod progress;
mod restore;
mod workspace;
mod wsl;

use clap::Parser;

pub(crate) type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = cli::Cli::parse();
    if !cfg!(windows) || std::env::var_os("WSL_DISTRO_NAME").is_some() {
        return Err("Run the Windows executable from PowerShell or CMD, not inside WSL".into());
    }
    let command = match args.command {
        Some(command) => command,
        None if args.non_interactive || args.json => return Err("A subcommand is required in non-interactive or JSON mode".into()),
        None => interactive::choose()?,
    };
    let progress = progress::Progress::new(args.no_progress || args.json);
    match command {
        cli::Command::List => {
            let names = wsl::installed_distros()?;
            if args.json { println!("{}", serde_json::to_string(&names)?); }
            else { for name in names { println!("{name}"); } }
        }
        cli::Command::Preview { distro, cleanup } => {
            cleanup.validate()?;
            let distro = wsl::check_distro(&distro)?;
            let paths = cleanup::run(&distro, cleanup::Mode::Preview, &cleanup)?;
            if args.json {
                println!("{}", serde_json::json!({"distro": distro, "paths": paths}));
            } else {
                println!("Generated directories in {distro} (nothing deleted):");
                for path in paths { println!("{path}"); }
            }
        }
        cli::Command::Backup(options) => {
            let outcome = backup::run(options, args.non_interactive || args.json, progress)?;
            if args.json {
                println!("{}", serde_json::json!({"distro": outcome.distro, "archive": outcome.path, "size_bytes": outcome.bytes}));
            } else {
                println!("Backup saved: {} ({} bytes)", outcome.path.display(), outcome.bytes);
            }
        }
        cli::Command::Config { action } => {
            config::execute(action, args.non_interactive, args.json)?;
        }
        cli::Command::Restore(options) => {
            let outcome = restore::run(options, progress)?;
            if args.json {
                println!("{}", serde_json::json!({"distro": outcome.name, "install_dir": outcome.directory}));
            } else {
                println!("Restored: {}. Start with: wsl -d \"{}\"", outcome.name, outcome.name);
            }
        }
    }
    Ok(())
}
