//! Restore an archive to a new distro name without replacing any user data.

use crate::{archive, cli::RestoreArgs, progress::Progress, workspace::Workspace, wsl, Result};
use std::{fs, path::PathBuf};

pub(crate) struct RestoreResult {
    pub(crate) name: String,
    pub(crate) directory: PathBuf,
}

pub(crate) fn run(options: RestoreArgs, progress: Progress) -> Result<RestoreResult> {
    if !options.archive.is_file() { return Err("Backup archive does not exist".into()); }
    wsl::ensure_missing(&options.name)?;
    if options.install_dir.exists() { return Err("Restore directory must not exist".into()); }
    let parent = options.install_dir.parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or("Restore directory needs a parent")?;
    fs::create_dir_all(parent)?;

    let work = Workspace::new(parent, &format!("restore-{}", std::process::id()))?;
    let tar = work.path.join("restore.tar");
    archive::decompress(&options.archive, &tar, progress)?;

    // WSL can register an incomplete distro before reporting an import error.
    if let Err(error) = progress.step("Importing restored distro", || {
        wsl::import(&options.name, &options.install_dir, &tar, options.version)
    }) {
        if wsl::installed_distros()?.iter().any(|d| d.eq_ignore_ascii_case(&options.name)) {
            eprintln!("warning: incomplete distro may exist: {}", options.name);
            eprintln!("warning: inspect it before unregistering manually");
        }
        return Err(error);
    }
    Ok(RestoreResult { name: options.name, directory: options.install_dir })
}
