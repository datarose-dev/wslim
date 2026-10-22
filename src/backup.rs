//! Backup orchestration. The original distro is exported, never cleaned.

use crate::{
    archive,
    cleanup::{self, Mode},
    cli::BackupArgs,
    config, interactive, output,
    progress::Progress,
    workspace::Workspace,
    wsl::{self, TemporaryDistros},
    Result,
};
use chrono::Local;
use inquire::Confirm;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub(crate) struct BackupResult {
    pub(crate) distro: String,
    pub(crate) path: PathBuf,
    pub(crate) bytes: u64,
}

pub(crate) fn run(
    options: BackupArgs,
    non_interactive: bool,
    progress: Progress,
) -> Result<BackupResult> {
    options.cleanup.validate()?;
    let distro = wsl::check_distro(&options.distro)?;
    let version = wsl::distro_version(&distro)?;
    let output_dir = output::resolve(options.output.clone(), non_interactive)?;
    let format = match options.format {
        Some(format) => format,
        None => config::backup_format()?,
    };
    let level = match options.level {
        Some(level) => level,
        None => config::backup_level()?,
    };
    let timestamp = Local::now().format("%Y-%m-%d-%H%M%S").to_string();
    let filename = format!(
        "{}-backup-{timestamp}.tar.{}",
        safe_name(&distro),
        format.extension()
    );
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
    let raw = work.path.join("original.tar");
    let cleaned = work.path.join("clean.tar");
    let compressed = work
        .path
        .join(format!("backup.tar.{}.partial", format.extension()));
    let stage_dir = work.path.join("stage");
    let test_dir = work.path.join("test");
    let mut temporary = TemporaryDistros::new(work);

    progress.step("Exporting source distro", || {
        wsl::run(&["--terminate", &distro])?;
        wsl::export(&distro, &raw)
    })?;

    let no_cleanup = options.cleanup.cleanup == [crate::cli::CleanupGroup::None];
    if no_cleanup {
        fs::rename(&raw, &cleaned)?;
    } else {
        progress.step("Importing disposable clone", || temporary.import(&stage, &stage_dir, &raw, version))?;
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
            temporary.import(&test, &test_dir, &cleaned, version)?;
            wsl::run(&["-d", &test, "-u", "root", "--", "/bin/sh", "-c", "exit 0"])?;
            temporary.unregister(&test)
        })?;
    } else {
        eprintln!("warning: restore boot-test skipped (--no-verify)");
    }

    archive::compress(&cleaned, &compressed, format, level, progress)?;
    progress.step("Checking archive integrity", || {
        archive::verify(&compressed, format)
    })?;

    // Publish the archive only after the boot test and checksum verification.
    let bytes = fs::metadata(&compressed)?.len();
    publish_no_clobber(&compressed, &destination)?;
    Ok(BackupResult {
        distro,
        path: destination,
        bytes,
    })
}

/// Resolve the existing source and destination parent without inspecting or
/// replacing the destination. Canonical Windows paths carry the extended prefix.
fn publication_paths(source: &Path, destination: &Path) -> io::Result<(PathBuf, PathBuf)> {
    let filename = destination.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Archive destination needs a filename",
        )
    })?;
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    Ok((
        fs::canonicalize(source)?,
        fs::canonicalize(parent)?.join(filename),
    ))
}

#[cfg(windows)]
fn publish_no_clobber(source: &Path, destination: &Path) -> io::Result<()> {
    publication::publish(source, destination)
}

#[cfg(windows)]
mod publication {
    use super::{publication_paths, Path};
    use std::{io, os::windows::ffi::OsStrExt};

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(source: *const u16, destination: *const u16, flags: u32) -> i32;
    }

    fn reject_nul(path: &Path) -> io::Result<()> {
        if path.as_os_str().encode_wide().any(|unit| unit == 0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Path contains an embedded NUL",
            ));
        }
        Ok(())
    }

    fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
        reject_nul(path)?;
        Ok(path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect())
    }

    pub(super) fn publish(source: &Path, destination: &Path) -> io::Result<()> {
        reject_nul(source)?;
        reject_nul(destination)?;
        let (source, destination) = publication_paths(source, destination)?;
        let source = wide_path(&source)?;
        let destination = wide_path(&destination)?;

        // SAFETY: Both UTF-16 buffers are NUL-terminated, contain no interior NUL,
        // and remain alive throughout the call. Flags 0 prohibit replacement and
        // cross-volume copying; failure leaves publication to the caller to report.
        let result = unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), 0) };
        if result == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

#[cfg(not(windows))]
fn publish_no_clobber(source: &Path, destination: &Path) -> io::Result<()> {
    let (source, destination) = publication_paths(source, destination)?;
    fs::hard_link(&source, &destination)?;
    // Publication already succeeded. A failed scratch unlink must not report
    // the completed archive as failed or invite an unsafe overwrite retry.
    if let Err(error) = fs::remove_file(&source) {
        eprintln!(
            "warning: archive published at {}; temporary file remains at {}: {error}",
            destination.display(),
            source.display()
        );
    }
    Ok(())
}

fn safe_name(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::test_support::TestDirectory;
    use std::{sync::Barrier, thread};
    #[test]
    fn filenames_are_safe_on_windows() {
        assert_eq!(safe_name("Ubuntu 24.04/Dev"), "Ubuntu-24.04-Dev");
    }

    #[test]
    fn publication_never_replaces_an_existing_archive() {
        let directory = TestDirectory::new();
        let source = directory.path.join("archive.partial");
        let destination = directory.path.join("archive.tar.xz");
        fs::write(&source, b"new archive").unwrap();
        fs::write(&destination, b"existing archive").unwrap();

        assert!(publish_no_clobber(&source, &destination).is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"existing archive");
        assert_eq!(fs::read(&source).unwrap(), b"new archive");
    }

    #[test]
    fn publication_never_replaces_an_existing_directory() {
        let directory = TestDirectory::new();
        let source = directory.path.join("archive.partial");
        let destination = directory.path.join("archive.tar.xz");
        fs::write(&source, b"new archive").unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("marker"), b"directory contents").unwrap();

        assert!(publish_no_clobber(&source, &destination).is_err());
        assert!(destination.is_dir());
        assert_eq!(
            fs::read(destination.join("marker")).unwrap(),
            b"directory contents"
        );
        assert_eq!(fs::read(&source).unwrap(), b"new archive");
    }

    #[test]
    fn publication_supports_unicode_paths() {
        let directory = TestDirectory::new();
        let parent = directory.path.join("árvíztűrő-工作");
        fs::create_dir(&parent).unwrap();
        let source = parent.join("archívum-备份.partial");
        let destination = parent.join("archívum-备份.tar.xz");
        fs::write(&source, b"unicode archive").unwrap();

        publish_no_clobber(&source, &destination).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"unicode archive");
        assert!(!source.exists());
    }

    #[test]
    fn concurrent_publication_has_exactly_one_winner() {
        let directory = TestDirectory::new();
        let first = directory.path.join("first.partial");
        let second = directory.path.join("second.partial");
        let destination = directory.path.join("archive.tar.xz");
        fs::write(&first, b"first archive").unwrap();
        fs::write(&second, b"second archive").unwrap();
        let barrier = Barrier::new(3);
        let results = thread::scope(|scope| {
            let first_writer = scope.spawn(|| {
                barrier.wait();
                publish_no_clobber(&first, &destination)
            });
            let second_writer = scope.spawn(|| {
                barrier.wait();
                publish_no_clobber(&second, &destination)
            });
            barrier.wait();
            [first_writer.join().unwrap(), second_writer.join().unwrap()]
        });

        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        let contents = fs::read(&destination).unwrap();
        if results[0].is_ok() {
            assert_eq!(contents, b"first archive");
            assert!(!first.exists());
            assert_eq!(fs::read(&second).unwrap(), b"second archive");
        } else {
            assert_eq!(contents, b"second archive");
            assert!(!second.exists());
            assert_eq!(fs::read(&first).unwrap(), b"first archive");
        }
    }

    #[cfg(windows)]
    #[test]
    fn publication_supports_extended_length_paths() {
        let directory = TestDirectory::new();
        let mut parent = fs::canonicalize(&directory.path).unwrap();
        for _ in 0..12 {
            parent = parent.join("extended-length-目录");
        }
        fs::create_dir_all(&parent).unwrap();
        let source = parent.join("archive.partial");
        let destination = parent.join("archive.tar.xz");
        assert!(destination.as_os_str().len() > 260);
        fs::write(&source, b"extended path archive").unwrap();

        publish_no_clobber(&source, &destination).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"extended path archive");
        assert!(!source.exists());
    }

    #[cfg(windows)]
    #[test]
    fn publication_rejects_embedded_utf16_nul() {
        use std::{ffi::OsString, os::windows::ffi::OsStringExt};

        let directory = TestDirectory::new();
        let source = directory.path.join("archive.partial");
        let destination = directory.path.join("archive.tar.xz");
        fs::write(&source, b"archive").unwrap();
        let invalid = PathBuf::from(OsString::from_wide(&[65, 0, 66]));

        assert_eq!(
            publish_no_clobber(&source, &invalid).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            publish_no_clobber(&invalid, &destination)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(!destination.exists());
        assert_eq!(fs::read(&source).unwrap(), b"archive");
    }
}
