//! All WSL process calls and disposable distro lifecycle management.

use crate::{workspace::Workspace, Result};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

pub(crate) fn run(args: &[&str]) -> Result<()> {
    capture(args)?;
    Ok(())
}

pub(crate) fn capture(args: &[&str]) -> Result<String> {
    let output = Command::new("wsl.exe").args(args).output()?;
    if !output.status.success() {
        let stderr = decode_output(&output.stderr);
        let stdout = decode_output(&output.stdout);
        let details = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        return Err(format!(
            "wsl.exe {} failed: {} {details}",
            args.first().unwrap_or(&""),
            output.status
        )
        .into());
    }
    Ok(decode_output(&output.stdout))
}

pub(crate) fn export(name: &str, path: &Path) -> Result<()> {
    run(&["--export", name, &path_arg(path)])
}

pub(crate) fn import(name: &str, dir: &Path, tar: &Path, version: u8) -> Result<()> {
    run(&[
        "--import",
        name,
        &path_arg(dir),
        &path_arg(tar),
        "--version",
        &version.to_string(),
    ])
}

pub(crate) fn installed_distros() -> Result<Vec<String>> {
    let output = Command::new("wsl.exe")
        .args(["--list", "--quiet"])
        .output()?;
    if !output.status.success() {
        return Err("Cannot list WSL distros. Is WSL installed?".into());
    }
    Ok(decode_output(&output.stdout)
        .lines()
        .map(|line| line.trim().trim_start_matches('\u{feff}').to_owned())
        .filter(|line| !line.is_empty())
        .collect())
}

pub(crate) fn check_distro(wanted: &str) -> Result<String> {
    installed_distros()?
        .into_iter()
        .find(|found| found.eq_ignore_ascii_case(wanted))
        .ok_or_else(|| format!("WSL distro not found: {wanted}").into())
}

pub(crate) fn ensure_missing(name: &str) -> Result<()> {
    if installed_distros()?
        .iter()
        .any(|distro| distro.eq_ignore_ascii_case(name))
    {
        return Err(format!("WSL distro already exists: {name}").into());
    }
    Ok(())
}

pub(crate) fn distro_version(name: &str) -> Result<u8> {
    let output = Command::new("wsl.exe")
        .args(["--list", "--verbose"])
        .output()?;
    if !output.status.success() {
        return Err("Cannot determine WSL version".into());
    }
    version_from_listing(&decode_output(&output.stdout), name)
        .ok_or_else(|| format!("Cannot determine WSL version of {name}").into())
}

fn path_arg(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn version_from_listing(listing: &str, name: &str) -> Option<u8> {
    for line in listing.lines() {
        let line = line.trim().trim_start_matches('*').trim();
        if let Some(rest) = line.strip_prefix(name) {
            if !rest.starts_with(char::is_whitespace) {
                continue;
            }
            if let Some(version) = rest.split_whitespace().last() {
                if version == "1" || version == "2" {
                    return version.parse().ok();
                }
            }
        }
    }
    None
}

// Redirected wsl.exe output is UTF-16LE on many Windows builds.
fn decode_output(bytes: &[u8]) -> String {
    let (pairs, _) = bytes.as_chunks::<2>();

    if bytes.starts_with(&[0xff, 0xfe]) || pairs.iter().take(64).any(|pair| pair[1] == 0) {
        let units = pairs
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect::<Vec<_>>();

        String::from_utf16_lossy(&units)
            .trim_start_matches('\u{feff}')
            .to_owned()
    } else {
        String::from_utf8_lossy(bytes)
            .trim_start_matches('\u{feff}')
            .to_string()
    }
}

/// Minimal lifecycle seam; discovery and unrelated WSL calls stay outside it.
pub(crate) trait DistroLifecycle {
    fn import(&mut self, name: &str, dir: &Path, tar: &Path, version: u8) -> Result<()>;
    fn terminate(&mut self, name: &str) -> Result<()>;
    fn unregister(&mut self, name: &str) -> Result<()>;
}

pub(crate) struct WslLifecycle;

impl DistroLifecycle for WslLifecycle {
    fn import(&mut self, name: &str, dir: &Path, tar: &Path, version: u8) -> Result<()> {
        import(name, dir, tar, version)
    }

    fn terminate(&mut self, name: &str) -> Result<()> {
        run(&["--terminate", name])
    }

    fn unregister(&mut self, name: &str) -> Result<()> {
        run(&["--unregister", name])
    }
}

struct ImportedDistro {
    name: String,
    directory: PathBuf,
}

/// Owns backing storage and only distros whose import invocation succeeded.
///
/// Automatic cleanup assumes no external process replaces a successfully
/// imported distro while this owner is alive. A matching name after a failed
/// import never establishes ownership and never authorizes destructive cleanup.
pub(crate) struct TemporaryDistros<B: DistroLifecycle = WslLifecycle> {
    workspace: Workspace,
    imported: Vec<ImportedDistro>,
    failed_import: bool,
    backend: B,
}

impl TemporaryDistros {
    pub(crate) fn new(workspace: Workspace) -> Self {
        Self {
            workspace,
            imported: Vec::new(),
            failed_import: false,
            backend: WslLifecycle,
        }
    }
}

impl<B: DistroLifecycle> TemporaryDistros<B> {
    pub(crate) fn import(&mut self, name: &str, dir: &Path, tar: &Path, version: u8) -> Result<()> {
        // A failed command can still leave registration or backing files behind.
        // Retain before invoking it, including if unwinding interrupts the call.
        self.workspace.retain();
        let previously_failed = self.failed_import;
        self.failed_import = true;
        match self.backend.import(name, dir, tar, version) {
            Ok(()) => {
                self.imported.push(ImportedDistro {
                    name: name.to_owned(),
                    directory: dir.to_owned(),
                });
                self.failed_import = previously_failed;
                Ok(())
            }
            Err(error) => {
                eprintln!(
                    "warning: import of temporary distro '{name}' in {} failed; ownership is unknown. \
                     Workspace retained at {} for inspection; no automatic cleanup will target this name",
                    dir.display(),
                    self.workspace.path.display()
                );
                Err(error)
            }
        }
    }

    pub(crate) fn unregister(&mut self, name: &str) -> Result<()> {
        if let Some(index) = self.imported.iter().position(|distro| distro.name == name) {
            self.remove_owned(index)?;
            self.release_if_safe();
        }
        Ok(())
    }

    fn remove_owned(&mut self, index: usize) -> Result<()> {
        let distro = &self.imported[index];
        if let Err(error) = self.backend.terminate(&distro.name) {
            self.warn_cleanup_failure(index, "terminate", error.as_ref());
            return Err(error);
        }
        if let Err(error) = self.backend.unregister(&distro.name) {
            self.warn_cleanup_failure(index, "unregister", error.as_ref());
            return Err(error);
        }
        self.imported.remove(index);
        Ok(())
    }

    fn warn_cleanup_failure(&self, index: usize, operation: &str, error: &dyn std::error::Error) {
        let distro = &self.imported[index];
        eprintln!(
            "warning: cannot {operation} temporary distro '{}' in {}: {error}. \
             Workspace retained at {}; inspect registration and files before taking action",
            distro.name,
            distro.directory.display(),
            self.workspace.path.display()
        );
    }

    fn release_if_safe(&mut self) {
        if self.imported.is_empty() && !self.failed_import {
            self.workspace.release();
        }
    }
}

impl<B: DistroLifecycle> Drop for TemporaryDistros<B> {
    fn drop(&mut self) {
        // Failed imports are never tracked. Retried cleanup targets only proven
        // successful imports and keeps storage if any cleanup remains unresolved.
        for index in (0..self.imported.len()).rev() {
            let _ = self.remove_owned(index);
        }
        self.release_if_safe();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::test_support::TestDirectory;
    use std::{cell::RefCell, collections::BTreeMap, fmt, fs, io, rc::Rc};

    #[derive(Debug, PartialEq, Eq)]
    enum Call {
        Import(String),
        Terminate(String),
        Unregister(String),
    }

    #[derive(Default, Clone, Copy)]
    enum ImportOutcome {
        #[default]
        Success,
        Failure {
            registered: bool,
        },
    }

    #[derive(Default)]
    struct FakeState {
        calls: Vec<Call>,
        installed: BTreeMap<String, PathBuf>,
        import_outcome: ImportOutcome,
        fail_terminate: bool,
        fail_unregister: bool,
    }

    struct FakeBackend(Rc<RefCell<FakeState>>);

    #[derive(Debug)]
    struct OriginalImportError(&'static str);

    impl fmt::Display for OriginalImportError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(self.0)
        }
    }

    impl std::error::Error for OriginalImportError {}

    impl DistroLifecycle for FakeBackend {
        fn import(&mut self, name: &str, dir: &Path, _tar: &Path, _version: u8) -> Result<()> {
            let mut state = self.0.borrow_mut();
            state.calls.push(Call::Import(name.to_owned()));
            if state.installed.contains_key(name) {
                return Err(Box::new(OriginalImportError("original name collision")));
            }
            fs::create_dir(dir)?;
            fs::write(dir.join("backing.vhdx"), b"backing storage")?;
            match state.import_outcome {
                ImportOutcome::Success => {
                    state.installed.insert(name.to_owned(), dir.to_owned());
                    Ok(())
                }
                ImportOutcome::Failure { registered } => {
                    if registered {
                        state.installed.insert(name.to_owned(), dir.to_owned());
                    }
                    Err(Box::new(OriginalImportError("original import failure")))
                }
            }
        }

        fn terminate(&mut self, name: &str) -> Result<()> {
            let mut state = self.0.borrow_mut();
            state.calls.push(Call::Terminate(name.to_owned()));
            if state.fail_terminate {
                return Err(io::Error::other("termination failed").into());
            }
            Ok(())
        }

        fn unregister(&mut self, name: &str) -> Result<()> {
            let mut state = self.0.borrow_mut();
            state.calls.push(Call::Unregister(name.to_owned()));
            if state.fail_unregister {
                return Err(io::Error::other("unregistration failed").into());
            }
            if let Some(path) = state.installed.remove(name) {
                fs::remove_dir_all(path)?;
            }
            Ok(())
        }
    }

    fn temporary(
        directory: &TestDirectory,
        state: Rc<RefCell<FakeState>>,
    ) -> TemporaryDistros<FakeBackend> {
        let workspace = Workspace::new(&directory.path, "lifecycle").unwrap();
        fs::write(workspace.path.join("original.tar"), b"source archive").unwrap();
        TemporaryDistros {
            workspace,
            imported: Vec::new(),
            failed_import: false,
            backend: FakeBackend(state),
        }
    }

    fn import_clone(temporary: &mut TemporaryDistros<FakeBackend>, name: &str) -> Result<()> {
        let directory = temporary.workspace.path.join(name);
        let tar = temporary.workspace.path.join("original.tar");
        temporary.import(name, &directory, &tar, 2)
    }

    #[test]
    fn failed_import_collision_never_cleans_matching_distro() {
        let directory = TestDirectory::new();
        let existing = directory.path.join("user-distro");
        fs::create_dir(&existing).unwrap();
        fs::write(existing.join("user-data"), b"untouched").unwrap();
        let state = Rc::new(RefCell::new(FakeState::default()));
        state
            .borrow_mut()
            .installed
            .insert("stage".into(), existing.clone());
        let mut temporary = temporary(&directory, Rc::clone(&state));
        let workspace = temporary.workspace.path.clone();

        let error = import_clone(&mut temporary, "stage").unwrap_err();
        assert_eq!(
            error.downcast_ref::<OriginalImportError>().unwrap().0,
            "original name collision"
        );
        temporary.unregister("stage").unwrap();
        drop(temporary);

        assert_eq!(state.borrow().calls, [Call::Import("stage".into())]);
        assert_eq!(fs::read(existing.join("user-data")).unwrap(), b"untouched");
        assert_eq!(
            fs::read(workspace.join("original.tar")).unwrap(),
            b"source archive"
        );
    }

    #[test]
    fn partial_and_unknown_failed_imports_keep_storage_and_original_error() {
        for registered in [false, true] {
            let directory = TestDirectory::new();
            let state = Rc::new(RefCell::new(FakeState {
                import_outcome: ImportOutcome::Failure { registered },
                ..FakeState::default()
            }));
            let mut temporary = temporary(&directory, Rc::clone(&state));
            let workspace = temporary.workspace.path.clone();
            let error = import_clone(&mut temporary, "stage").unwrap_err();
            assert_eq!(
                error.downcast_ref::<OriginalImportError>().unwrap().0,
                "original import failure"
            );
            drop(temporary);

            assert_eq!(state.borrow().calls, [Call::Import("stage".into())]);
            assert_eq!(
                fs::read(workspace.join("stage/backing.vhdx")).unwrap(),
                b"backing storage"
            );
            assert_eq!(
                fs::read(workspace.join("original.tar")).unwrap(),
                b"source archive"
            );
            assert_eq!(state.borrow().installed.contains_key("stage"), registered);
        }
    }

    #[test]
    fn cleanup_failures_keep_successfully_imported_backing_storage() {
        for fail_terminate in [false, true] {
            let directory = TestDirectory::new();
            let state = Rc::new(RefCell::new(FakeState {
                fail_terminate,
                fail_unregister: !fail_terminate,
                ..FakeState::default()
            }));
            let mut temporary = temporary(&directory, Rc::clone(&state));
            let workspace = temporary.workspace.path.clone();
            import_clone(&mut temporary, "stage").unwrap();
            assert!(temporary.unregister("stage").is_err());
            drop(temporary);

            assert_eq!(
                fs::read(workspace.join("stage/backing.vhdx")).unwrap(),
                b"backing storage"
            );
            assert_eq!(
                fs::read(workspace.join("original.tar")).unwrap(),
                b"source archive"
            );
            let mut expected = vec![
                Call::Import("stage".into()),
                Call::Terminate("stage".into()),
            ];
            if !fail_terminate {
                expected.push(Call::Unregister("stage".into()));
            }
            expected.push(Call::Terminate("stage".into()));
            if !fail_terminate {
                expected.push(Call::Unregister("stage".into()));
            }
            assert_eq!(state.borrow().calls, expected);
            assert!(state.borrow().installed.contains_key("stage"));
        }
    }

    #[test]
    fn all_successful_imports_allow_workspace_removal() {
        let directory = TestDirectory::new();
        let state = Rc::new(RefCell::new(FakeState::default()));
        let mut temporary = temporary(&directory, Rc::clone(&state));
        let workspace = temporary.workspace.path.clone();
        import_clone(&mut temporary, "stage").unwrap();
        temporary.unregister("stage").unwrap();
        import_clone(&mut temporary, "test").unwrap();
        temporary.unregister("test").unwrap();
        drop(temporary);

        assert!(!workspace.exists());
        assert!(state.borrow().installed.is_empty());
        assert_eq!(
            state.borrow().calls,
            [
                Call::Import("stage".into()),
                Call::Terminate("stage".into()),
                Call::Unregister("stage".into()),
                Call::Import("test".into()),
                Call::Terminate("test".into()),
                Call::Unregister("test".into()),
            ]
        );
    }

    #[test]
    fn early_return_cleans_only_owned_imports_before_workspace_removal() {
        fn return_early(mut temporary: TemporaryDistros<FakeBackend>) -> Result<()> {
            import_clone(&mut temporary, "stage")?;
            Err("later backup error".into())
        }

        let directory = TestDirectory::new();
        let source = directory.path.join("source-distro");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("user-data"), b"source untouched").unwrap();
        let state = Rc::new(RefCell::new(FakeState::default()));
        state
            .borrow_mut()
            .installed
            .insert("source".into(), source.clone());
        let temporary = temporary(&directory, Rc::clone(&state));
        let workspace = temporary.workspace.path.clone();

        assert_eq!(
            return_early(temporary).unwrap_err().to_string(),
            "later backup error"
        );
        assert!(!workspace.exists());
        assert_eq!(
            fs::read(source.join("user-data")).unwrap(),
            b"source untouched"
        );
        assert_eq!(
            state.borrow().calls,
            [
                Call::Import("stage".into()),
                Call::Terminate("stage".into()),
                Call::Unregister("stage".into()),
            ]
        );
    }

    #[test]
    fn failed_second_import_retains_workspace_after_owned_cleanup() {
        for remove_stage_first in [false, true] {
            let directory = TestDirectory::new();
            let state = Rc::new(RefCell::new(FakeState::default()));
            let mut temporary = temporary(&directory, Rc::clone(&state));
            let workspace = temporary.workspace.path.clone();
            import_clone(&mut temporary, "stage").unwrap();
            if remove_stage_first {
                temporary.unregister("stage").unwrap();
            }
            state.borrow_mut().import_outcome = ImportOutcome::Failure { registered: true };
            assert!(import_clone(&mut temporary, "test").is_err());
            drop(temporary);

            assert!(workspace.exists());
            assert!(!workspace.join("stage").exists());
            assert_eq!(
                fs::read(workspace.join("test/backing.vhdx")).unwrap(),
                b"backing storage"
            );
            assert!(state.borrow().installed.contains_key("test"));
            let expected = if remove_stage_first {
                [
                    Call::Import("stage".into()),
                    Call::Terminate("stage".into()),
                    Call::Unregister("stage".into()),
                    Call::Import("test".into()),
                ]
            } else {
                [
                    Call::Import("stage".into()),
                    Call::Import("test".into()),
                    Call::Terminate("stage".into()),
                    Call::Unregister("stage".into()),
                ]
            };
            assert_eq!(state.borrow().calls, expected);
        }
    }

    #[test]
    fn parses_utf16_wsl_output() {
        let bytes: Vec<u8> = "dev\r\nUbuntu\r\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(decode_output(&bytes), "dev\r\nUbuntu\r\n");
    }

    #[test]
    fn parses_wsl_version_without_prefix_collision() {
        let listing =
            "  NAME       STATE     VERSION\n* dev-old    Stopped   1\n  dev        Running   2\n";
        assert_eq!(version_from_listing(listing, "dev"), Some(2));
    }
}
