//! Backup orchestration. The original distro is exported, never cleaned.

use crate::{
    archive,
    cleanup::{self, Mode},
    cli::BackupArgs,
    config,
    interactive,
    output,
    progress::Progress,
    workspace::Workspace,
    wsl::{self, TemporaryDistros},
    Result,
};
use chrono::Local;
use inquire::Confirm;
use std::{fs, path::PathBuf};

pub(crate) struct BackupResult {
    pub(crate) distro: String,
    pub(crate) path: PathBuf,
    pub(crate) bytes: u64,
}

pub(crate) fn run(options: BackupArgs, non_interactive: bool, progress: Progress) -> Result<BackupResult> {
    options.cleanup.validate()?;
    let distro = wsl::check_distro(&options.distro)?;
    let version = wsl::distro_version(&distro)?;
    let output_dir = output::resolve(options.output.clone(), non_interactive)?;
    let format = match options.format { Some(format) => format, None => config::backup_format()? };
    let level = match options.level { Some(level) => level, None => config::backup_level()? };
    let timestamp = Local::now().format("%Y-%m-%d-%H%M%S").to_string();
    let filename = format!("{}-backup-{timestamp}.tar.{}", safe_name(&distro), format.extension());
    let destination = output_dir.join(filename);
    if destination.exists() {
        return Err(format!("Backup already exists: {}", destination.display()).into());
    }

    if !options.yes {
        if non_interactive {
            return Err("--yes is required for backup in --non-interactive mode".into());
        }
        interactive::ensure_terminal()?;
        eprintln!("Backup of '{distro}' (WSL {version}) will terminate its running processes.");
        eprintln!("Cleanup changes only a temporary clone; --cleanup none exports the source directly.");
        if !Confirm::new("Continue?").with_default(false).prompt()? {
            return Err("Backup cancelled; nothing changed".into());
        }
    }

    let id = format!("{timestamp}-{}", std::process::id());
    let work = Workspace::new(&output_dir, &id)?;
    let stage = format!("wslim-stage-{id}");
    let test = format!("wslim-test-{id}");
    wsl::ensure_missing(&stage)?;
    wsl::ensure_missing(&test)?;
    let mut temporary = TemporaryDistros::default();

    let raw = work.path.join("original.tar");
    let cleaned = work.path.join("clean.tar");
    let compressed = work.path.join(format!("backup.tar.{}.partial", format.extension()));

    progress.step("Exporting source distro", || {
        wsl::run(&["--terminate", &distro])?;
        wsl::export(&distro, &raw)
    })?;

    let no_cleanup = options.cleanup.cleanup == [crate::cli::CleanupGroup::None];
    if no_cleanup {
        fs::rename(&raw, &cleaned)?;
    } else {
        progress.step("Importing disposable clone", || temporary.import(&stage, &work.path.join("stage"), &raw, version))?;
        fs::remove_file(&raw)?;

        progress.step("Removing generated dependencies from the clone", || -> Result<()> {
            let removed = cleanup::run(&stage, Mode::Clean, &options.cleanup)?;
            eprintln!("Cleanup: {} directories", removed.len());
            wsl::run(&["--terminate", &stage])
        })?;

        progress.step("Exporting cleaned clone", || wsl::export(&stage, &cleaned))?;

        // The cleaned TAR no longer depends on the imported clone. Free its disk
        // before creating the test distro to keep peak temporary storage lower.
        progress.step("Removing disposable clone", || temporary.unregister(&stage))?;
    }

    if !options.no_verify {
        progress.step("Testing the restored distro", || -> Result<()> {
            temporary.import(&test, &work.path.join("test"), &cleaned, version)?;
            wsl::run(&["-d", &test, "-u", "root", "--", "/bin/sh", "-c", "exit 0"])?;
            temporary.unregister(&test)
        })?;
    } else {
        eprintln!("warning: restore boot-test skipped (--no-verify)");
    }

    archive::compress(&cleaned, &compressed, format, level, progress)?;
    progress.step("Checking archive integrity", || archive::verify(&compressed, format))?;

    // Publish the archive only after the boot test and checksum verification.
    fs::rename(&compressed, &destination)?;
    let bytes = fs::metadata(&destination)?.len();
    Ok(BackupResult { distro, path: destination, bytes })
}

fn safe_name(name: &str) -> String {
    name.chars().map(|ch| {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') { ch } else { '-' }
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filenames_are_safe_on_windows() {
        assert_eq!(safe_name("Ubuntu 24.04/Dev"), "Ubuntu-24.04-Dev");
    }
}
