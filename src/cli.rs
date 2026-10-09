//! Typed CLI contract shared by interactive use and external frontends.

use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "wslim", version, about = "Restorable, space-efficient WSL backups")]
pub(crate) struct Cli {
    /// Never prompt for input. Missing parameters or approval are errors.
    #[arg(long, global = true)]
    pub(crate) non_interactive: bool,

    /// Disable terminal progress indicators (useful for scripts and GUIs).
    #[arg(long, global = true)]
    pub(crate) no_progress: bool,

    /// Print one machine-readable JSON result on stdout.
    #[arg(long, global = true)]
    pub(crate) json: bool,

    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// List registered WSL distributions.
    List,
    /// Preview directories that a backup would omit.
    Preview {
        distro: String,
        #[command(flatten)]
        cleanup: CleanupOptions,
    },
    /// Back up a distro without modifying its files.
    Backup(BackupArgs),
    /// Restore an archive to a NEW WSL distribution.
    Restore(RestoreArgs),
    /// View or edit persistent application settings.
    Config {
        #[command(subcommand)]
        action: Option<ConfigCommand>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum CompressionFormat {
    Xz,
    Gz,
}

impl CompressionFormat {
    pub(crate) fn as_str(self) -> &'static str {
        match self { Self::Xz => "xz", Self::Gz => "gz" }
    }

    pub(crate) fn label(self) -> &'static str {
        match self { Self::Xz => "XZ", Self::Gz => "Gzip" }
    }

    pub(crate) fn extension(self) -> &'static str { self.as_str() }

    pub(crate) fn from_archive_path(path: &std::path::Path) -> crate::Result<Self> {
        let path = path.to_string_lossy().to_ascii_lowercase();
        if path.ends_with(".tar.xz") { Ok(Self::Xz) }
        else if path.ends_with(".tar.gz") { Ok(Self::Gz) }
        else { Err("Unsupported archive extension (expected .tar.xz or .tar.gz)".into()) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ConfigKey {
    #[value(name = "backup.output")]
    BackupOutput,
    #[value(name = "backup.format")]
    BackupFormat,
    #[value(name = "backup.level")]
    BackupLevel,
}

impl ConfigKey {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::BackupOutput => "backup.output",
            Self::BackupFormat => "backup.format",
            Self::BackupLevel => "backup.level",
        }
    }
}

#[derive(Debug, Subcommand)]
pub(crate) enum ConfigCommand {
    /// Show all supported settings and their current values.
    List,
    /// Print the value of a setting.
    Get { key: ConfigKey },
    /// Store a setting for future runs.
    Set { key: ConfigKey, value: String },
    /// Shortcut: wslim config backup.output D:\\Backups
    #[command(name = "backup.output")]
    Output { value: PathBuf },
    /// Clear a previously saved setting.
    Unset { key: ConfigKey },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum CleanupGroup {
    Node,
    Php,
    Rust,
    Caches,
    None,
}

impl CleanupGroup {
    fn flag(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Php => "php",
            Self::Rust => "rust",
            Self::Caches => "caches",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, Args)]
pub(crate) struct CleanupOptions {
    /// Cleanup groups: node,php,rust,caches or none.
    #[arg(long, value_enum, value_delimiter = ',', default_value = "node,php,rust,caches")]
    pub(crate) cleanup: Vec<CleanupGroup>,
}

impl CleanupOptions {
    pub(crate) fn validate(&self) -> crate::Result<()> {
        if self.cleanup.contains(&CleanupGroup::None) && self.cleanup.len() != 1 {
            return Err("--cleanup none cannot be combined with other cleanup groups".into());
        }
        Ok(())
    }

    pub(crate) fn flags(&self) -> String {
        self.cleanup.iter().map(|group| group.flag()).collect::<Vec<_>>().join(",")
    }
}

#[derive(Debug, Args)]
pub(crate) struct BackupArgs {
    /// Name of the distro to back up.
    pub(crate) distro: String,

    /// Directory for the final timestamped .tar.xz or .tar.gz archive.
    #[arg(short, long, alias = "target")]
    pub(crate) output: Option<PathBuf>,

    /// Approve terminating the source distro. Required in non-interactive mode.
    #[arg(short = 'y', long)]
    pub(crate) yes: bool,

    /// Skip test-import and shell boot check (archive integrity is still checked).
    #[arg(long)]
    pub(crate) no_verify: bool,

    /// Archive format (xz or gz); defaults to saved backup.format or xz.
    #[arg(long, value_enum)]
    pub(crate) format: Option<CompressionFormat>,

    /// Compression level (0-9); defaults to saved backup.level or 6.
    #[arg(long, alias = "xz-level", value_parser = clap::value_parser!(u8).range(0..=9))]
    pub(crate) level: Option<u8>,

    #[command(flatten)]
    pub(crate) cleanup: CleanupOptions,
}

#[derive(Debug, Args)]
pub(crate) struct RestoreArgs {
    /// Path to an existing wslim .tar.xz or .tar.gz archive.
    pub(crate) archive: PathBuf,

    /// New distro name; must not exist.
    #[arg(long = "as")]
    pub(crate) name: String,

    /// New directory for the restored distro; must not exist.
    #[arg(long)]
    pub(crate) install_dir: PathBuf,

    /// WSL version for the imported distro.
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u8).range(1..=2))]
    pub(crate) version: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_backup_options_are_scriptable() {
        let args = Cli::try_parse_from([
            "wslim", "--non-interactive", "--no-progress", "--json", "backup", "dev",
            "--output", "D:\\Backups", "--yes", "--no-verify", "--format", "gz", "--level", "3",
            "--cleanup", "node,rust",
        ]).unwrap();
        assert!(args.non_interactive && args.no_progress && args.json);
        let Some(Command::Backup(backup)) = args.command else { panic!("expected backup"); };
        assert_eq!(backup.level, Some(3));
        assert_eq!(backup.format, Some(CompressionFormat::Gz));
        assert_eq!(backup.cleanup.cleanup, vec![CleanupGroup::Node, CleanupGroup::Rust]);
    }

    #[test]
    fn rejects_unknown_or_invalid_options() {
        assert!(Cli::try_parse_from(["wslim", "backup", "dev", "--output", "x", "--unknown"]).is_err());
        assert!(Cli::try_parse_from(["wslim", "backup", "dev", "--output", "x", "--level", "10"]).is_err());
        assert!(Cli::try_parse_from(["wslim", "restore", "backup.tar.xz", "--as", "dev"]).is_err());
        assert!(Cli::try_parse_from(["wslim", "config", "set", "bad.key", "somewhere"]).is_err());
    }

    #[test]
    fn backup_destination_can_come_from_config() {
        let parsed = Cli::try_parse_from(["wslim", "backup", "dev", "--yes", "--non-interactive"]).unwrap();
        let Some(Command::Backup(backup)) = parsed.command else { panic!("expected backup"); };
        assert!(backup.output.is_none());
    }

    #[test]
    fn target_alias_is_available_for_scripts() {
        let parsed = Cli::try_parse_from(["wslim", "backup", "dev", "--target", "D:\\Backups"]).unwrap();
        let Some(Command::Backup(backup)) = parsed.command else { panic!("expected backup"); };
        assert_eq!(backup.output, Some(PathBuf::from("D:\\Backups")));
    }

    #[test]
    fn backward_compatible_xz_level_alias() {
        let parsed = Cli::try_parse_from(["wslim", "backup", "dev", "--xz-level", "3"]).unwrap();
        let Some(Command::Backup(backup)) = parsed.command else { panic!("expected backup"); };
        assert_eq!(backup.level, Some(3));
    }

    #[test]
    fn archive_extensions() {
        assert_eq!(CompressionFormat::from_archive_path(std::path::Path::new("backup.tar.gz")).unwrap(), CompressionFormat::Gz);
        assert_eq!(CompressionFormat::from_archive_path(std::path::Path::new("backup.tar.xz")).unwrap(), CompressionFormat::Xz);
        assert!(CompressionFormat::from_archive_path(std::path::Path::new("backup.zip")).is_err());
    }

    #[test]
    fn config_set_parses_dotted_key() {
        let parsed = Cli::try_parse_from(["wslim", "config", "set", "backup.output", "D:\\WSLBackups"]).unwrap();
        assert!(matches!(parsed.command, Some(Command::Config { action: Some(ConfigCommand::Set { key: ConfigKey::BackupOutput, .. }) })));
    }

    #[test]
    fn config_shortcut_parses() {
        let parsed = Cli::try_parse_from(["wslim", "config", "backup.output", "D:\\Backup"]).unwrap();
        assert!(matches!(parsed.command, Some(Command::Config { action: Some(ConfigCommand::Output { .. }) })));
    }

    #[test]
    fn none_cleanup_must_be_exclusive() {
        let opts = CleanupOptions { cleanup: vec![CleanupGroup::None, CleanupGroup::Node] };
        assert!(opts.validate().is_err());
    }
}
