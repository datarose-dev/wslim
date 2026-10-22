//! A small optional wizard. Explicit subcommands always bypass it.

use crate::{cli::{BackupArgs, CleanupGroup, CleanupOptions, Command, CompressionFormat, RestoreArgs}, config, wsl, Result};
use inquire::{Confirm, MultiSelect, Select, Text};
use std::{io::{self, IsTerminal}, path::PathBuf};

pub(crate) fn ensure_terminal() -> Result<()> {
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return Err("Interactive prompts require a terminal; pass a subcommand and all options".into());
    }
    Ok(())
}

pub(crate) fn choose() -> Result<Command> {
    ensure_terminal()?;
    let action = Select::new("What would you like to do?", vec!["backup", "preview", "restore", "list", "config"]).prompt()?;
    match action {
        "list" => Ok(Command::List),
        "preview" => Ok(Command::Preview { distro: select_distro()?, cleanup: select_cleanup()? }),
        "backup" => {
            let distro = select_distro()?;
            let cleanup = select_cleanup()?;
            let verify = Confirm::new("Test-restore the archive?").with_default(true).prompt()?;
            let format = select_format()?;
            let level = select_level(config::backup_level()?)?;
            Ok(Command::Backup(BackupArgs {
                distro, output: None, yes: false, no_verify: !verify,
                format: Some(format), level: Some(level), cleanup,
            }))
        }
        "config" => Ok(Command::Config { action: None }),
        "restore" => {
            let archive = PathBuf::from(Text::new("Archive (.tar.xz or .tar.gz):").prompt()?);
            let name = Text::new("New distro name:").prompt()?;
            let install_dir = PathBuf::from(Text::new("Install directory:").prompt()?);
            let version = Select::new("WSL version:", vec!["2", "1"]).prompt()?.parse()?;
            Ok(Command::Restore(RestoreArgs { archive, name, install_dir, version }))
        }
        _ => unreachable!("inquire returned an option outside the provided menu"),
    }
}

/// Descriptive defaults for the interactive menu; the CLI still accepts 0–9.
#[derive(Clone, Copy)]
struct CompressionPreset {
    level: u8,
    description: &'static str,
}

impl std::fmt::Display for CompressionPreset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} - {}", self.level, self.description)
    }
}

fn select_format() -> Result<CompressionFormat> {
    let default = config::backup_format()?;
    let names = vec!["xz", "gz"];
    let selected = Select::new("Compression format:", names)
        .with_starting_cursor(usize::from(default == CompressionFormat::Gz))
        .prompt()?;
    Ok(if selected == "gz" { CompressionFormat::Gz } else { CompressionFormat::Xz })
}

fn select_level(default: u8) -> Result<u8> {
    let presets = vec![
        CompressionPreset { level: 1, description: "Fast (larger archive)" },
        CompressionPreset { level: 3, description: "Standard (good speed)" },
        CompressionPreset { level: 6, description: "Balanced (recommended)" },
        CompressionPreset { level: 9, description: "Maximum (slow, high memory usage)" },
    ];

    // Preserve the configured default without reordering the options.
    let selected = match default { 1 => 0, 3 => 1, 9 => 3, _ => 2 };
    Ok(Select::new("Compression level:", presets)
        .with_starting_cursor(selected)
        .prompt()?
        .level)
}

fn select_distro() -> Result<String> {
    let names = wsl::installed_distros()?;
    if names.is_empty() {
        return Err("No WSL distributions found".into());
    }
    Ok(Select::new("WSL distribution:", names).prompt()?)
}

fn select_cleanup() -> Result<CleanupOptions> {
    let groups = vec![CleanupGroup::Node, CleanupGroup::Php, CleanupGroup::Rust, CleanupGroup::Caches];
    let chosen = MultiSelect::new("Remove generated dependencies from a temporary clone (clear selection to export source directly):", groups)
        .with_all_selected_by_default()
        .prompt()?;
    Ok(CleanupOptions { cleanup: if chosen.is_empty() { vec![CleanupGroup::None] } else { chosen } })
}

// Inquire renders options with Display. Keep the enum spellings stable for scripts.
impl std::fmt::Display for CleanupGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Node => "node_modules (Node.js)",
            Self::Php => "vendor (Composer)",
            Self::Rust => "target (Rust)",
            Self::Caches => "package manager caches",
            Self::None => "none",
        })
    }
}
