//! Resolve and verify the backup destination before stopping the source distro.

use crate::{config, interactive, Result};
use inquire::{Confirm, Text};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

const GIB: u64 = 1024 * 1024 * 1024;

pub(crate) fn budget(bytes: u64) -> Result<u64> {
    let quarter = bytes.checked_add(3).ok_or("Disk-space estimate overflow")? / 4;
    bytes
        .checked_add(quarter.max(GIB))
        .ok_or_else(|| "Disk-space estimate overflow".into())
}

pub(crate) fn available(path: &Path) -> Result<u64> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetDiskFreeSpaceExW(
                path: *const u16,
                available: *mut u64,
                total: *mut u64,
                free: *mut u64,
            ) -> i32;
        }
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut bytes = 0;
        // SAFETY: wide is NUL-terminated and output pointer is valid.
        if unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut bytes,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(bytes)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err("Disk-space checks require Windows".into())
    }
}

pub(crate) fn check_space(
    path: &Path,
    stage: &str,
    bytes: u64,
    non_interactive: bool,
) -> Result<()> {
    let estimate = budget(bytes)?;
    let free = available(path)?;
    if free >= estimate {
        return Ok(());
    }
    eprintln!("warning: {stage} at '{}' estimates {estimate} bytes; {free} bytes available. This heuristic does not guarantee sufficient space.", path.display());
    if non_interactive {
        return Err(
            "Insufficient estimated disk space; non-interactive runs cannot override".into(),
        );
    }
    interactive::ensure_terminal()?;
    if !Confirm::new("Continue despite estimated disk-space shortfall?")
        .with_default(false)
        .prompt()?
    {
        return Err("Backup cancelled after disk-space warning".into());
    }
    Ok(())
}

pub(crate) fn resolve(explicit: Option<PathBuf>, non_interactive: bool) -> Result<PathBuf> {
    // Explicit arguments always override the saved preference.
    let mut choice = match explicit {
        Some(path) => Some(path),
        None => config::backup_output()?,
    };
    loop {
        let path = match choice.take() {
            Some(path) => path,
            None if non_interactive => {
                return Err("No backup destination. Pass --output DIR or set 'wslim config set backup.output DIR'".into());
            }
            None => prompt(None)?,
        };
        match check_writable(&path) {
            Ok(valid) => return Ok(valid),
            Err(error) if non_interactive => {
                return Err(format!(
                    "Backup destination '{}' is unavailable: {error}",
                    path.display()
                )
                .into());
            }
            Err(error) => {
                eprintln!(
                    "Backup destination '{}' is unavailable: {error}",
                    path.display()
                );
                eprintln!("No WSL distribution has been stopped or modified.");
                choice = Some(prompt(Some(&path))?);
            }
        }
    }
}

fn prompt(previous: Option<&Path>) -> Result<PathBuf> {
    interactive::ensure_terminal()?;
    let initial = previous
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut question = Text::new("Windows backup directory:");
    if !initial.is_empty() {
        question = question.with_default(&initial);
    }
    let text = question.prompt()?;
    if text.trim().is_empty() {
        return Err("Backup destination cannot be empty".into());
    }
    Ok(PathBuf::from(text.trim()))
}

fn check_writable(path: &Path) -> Result<PathBuf> {
    // A missing directory is fine on an existing drive: create it if possible.
    fs::create_dir_all(path)?;
    let directory = fs::canonicalize(path)?;
    if !directory.is_dir() {
        return Err(format!("{} is not a directory", directory.display()).into());
    }

    // A read-only mount may allow directory listing but reject writes.
    // Use create_new to avoid touching any existing user files.
    for attempt in 0..8 {
        let probe = directory.join(format!(
            ".wslim-write-test-{}-{attempt}",
            std::process::id()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&probe) {
            Ok(mut file) => {
                let tested = file.write_all(b"wslim");
                drop(file);
                let cleanup = fs::remove_file(&probe);
                tested?;
                cleanup?;
                return Ok(directory);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err("Could not create a unique destination write-test file".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writable_directory_is_created_and_cleaned() {
        let folder = std::env::temp_dir().join(format!("wslim-output-test-{}", std::process::id()));
        let nested = folder.join("nested");
        let actual = check_writable(&nested).unwrap();
        assert_eq!(actual, fs::canonicalize(&nested).unwrap());
        assert_eq!(fs::read_dir(&nested).unwrap().count(), 0);
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn regular_file_cannot_be_destination() {
        let file = std::env::temp_dir().join(format!("wslim-output-file-{}", std::process::id()));
        fs::write(&file, b"not a directory").unwrap();
        assert!(check_writable(&file).is_err());
        fs::remove_file(file).unwrap();
    }

    #[test]
    fn disk_budget_uses_quarter_or_one_gibibyte_margin() {
        assert_eq!(budget(0).unwrap(), GIB);
        assert_eq!(budget(GIB * 8).unwrap(), GIB * 10);
    }
}
