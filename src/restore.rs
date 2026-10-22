//! Restore an archive to a new distro name without replacing any user data.

use crate::{archive, cli::RestoreArgs, progress::Progress, workspace::Workspace, wsl, Result};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(crate) struct RestoreResult {
    pub(crate) name: String,
    pub(crate) directory: PathBuf,
}

pub(crate) fn run(options: RestoreArgs, progress: Progress) -> Result<RestoreResult> {
    if !options.archive.is_file() {
        return Err("Backup archive does not exist".into());
    }
    wsl::ensure_missing(&options.name)?;
    if options.install_dir.exists() {
        return Err("Restore directory must not exist".into());
    }
    let parent = options
        .install_dir
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or("Restore directory needs a parent")?;
    fs::create_dir_all(parent)?;

    let work = Workspace::new(parent, &format!("restore-{}", std::process::id()))?;
    let tar = work.path.join("restore.tar");
    archive::decompress(&options.archive, &tar, progress)?;

    let result = progress.step("Importing restored distro", || {
        wsl::import(&options.name, &options.install_dir, &tar, options.version)
    });
    finish_import(
        result,
        &options.name,
        &options.install_dir,
        wsl::installed_distros,
    )?;
    Ok(RestoreResult {
        name: options.name,
        directory: options.install_dir,
    })
}

fn finish_import(
    result: Result<()>,
    name: &str,
    directory: &Path,
    installed: impl FnOnce() -> Result<Vec<String>>,
) -> Result<()> {
    if let Err(error) = result {
        eprintln!(
            "warning: import of '{name}' failed; requested directory {} is left for inspection. \
             No automatic cleanup was attempted",
            directory.display()
        );
        match installed() {
            Ok(names) if names.iter().any(|distro| distro.eq_ignore_ascii_case(name)) => {
                eprintln!(
                    "warning: a distro named '{name}' is registered, but its ownership is unknown. \
                     Inspect the distro and requested directory before taking action"
                );
            }
            Err(diagnostic_error) => {
                eprintln!(
                    "warning: cannot check whether '{name}' is registered: {diagnostic_error}"
                );
            }
            Ok(_) => {}
        }
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::test_support::TestDirectory;
    use std::io;

    #[test]
    fn failed_restore_keeps_original_error_when_diagnostics_fail() {
        let directory = TestDirectory::new();
        let workspace = Workspace::new(&directory.path, "restore").unwrap();
        let scratch = workspace.path.clone();
        fs::write(scratch.join("restore.tar"), b"restore archive").unwrap();
        let install = directory.path.join("restore");
        fs::create_dir(&install).unwrap();
        fs::write(install.join("backing.vhdx"), b"partial restore").unwrap();
        let original = io::Error::new(io::ErrorKind::PermissionDenied, "original import error");

        let error = finish_import(Err(original.into()), "restore", &install, || {
            Err(io::Error::other("diagnostic lookup failed").into())
        })
        .unwrap_err();
        drop(workspace);

        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(error.to_string(), "original import error");
        assert!(!scratch.exists());
        assert_eq!(
            fs::read(install.join("backing.vhdx")).unwrap(),
            b"partial restore"
        );
    }

    #[test]
    fn failed_restore_keeps_directory_when_matching_name_is_registered() {
        let directory = TestDirectory::new();
        let install = directory.path.join("restore");
        fs::create_dir(&install).unwrap();
        fs::write(install.join("backing.vhdx"), b"partial restore").unwrap();

        let error = finish_import(
            Err("original import error".into()),
            "restore",
            &install,
            || Ok(vec!["RESTORE".into()]),
        )
        .unwrap_err();

        assert_eq!(error.to_string(), "original import error");
        assert_eq!(
            fs::read(install.join("backing.vhdx")).unwrap(),
            b"partial restore"
        );
    }

    #[test]
    fn successful_restore_does_not_run_diagnostics() {
        let directory = TestDirectory::new();
        finish_import(Ok(()), "restore", &directory.path, || {
            panic!("successful import must not query diagnostics")
        })
        .unwrap();
    }
}
