//! Remove scratch files unless unresolved distro ownership requires inspection.

use crate::Result;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(crate) struct Workspace {
    pub(crate) path: PathBuf,
    retained: bool,
}

impl Workspace {
    pub(crate) fn new(parent: &Path, id: &str) -> Result<Self> {
        let path = parent.join(format!(".wslim-work-{id}"));
        fs::create_dir(&path)?;
        Ok(Self {
            path,
            retained: false,
        })
    }

    /// Keep backing files until the distro lifecycle owner proves deletion safe.
    pub(crate) fn retain(&mut self) {
        self.retained = true;
    }

    /// Resume cleanup only after all owned imports are removed, with no failed import.
    pub(crate) fn release(&mut self) {
        self.retained = false;
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        if self.retained {
            return;
        }
        if let Err(error) = fs::remove_dir_all(&self.path) {
            eprintln!(
                "warning: temporary files remain in {}: {error}",
                self.path.display()
            );
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    pub(crate) struct TestDirectory {
        pub(crate) path: PathBuf,
    }

    impl TestDirectory {
        pub(crate) fn new() -> Self {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "wslim-lifecycle-test-{}-{stamp}-{id}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.path).unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{test_support::TestDirectory, *};

    #[test]
    fn ordinary_workspace_is_removed_on_drop() {
        let directory = TestDirectory::new();
        let workspace = Workspace::new(&directory.path, "ordinary").unwrap();
        let path = workspace.path.clone();
        fs::write(path.join("scratch.tar"), b"scratch").unwrap();
        drop(workspace);
        assert!(!path.exists());
    }

    #[test]
    fn retained_workspace_is_not_removed_on_drop() {
        let directory = TestDirectory::new();
        let mut workspace = Workspace::new(&directory.path, "retained").unwrap();
        let path = workspace.path.clone();
        fs::write(path.join("backing.vhdx"), b"backing storage").unwrap();
        workspace.retain();
        drop(workspace);
        assert_eq!(
            fs::read(path.join("backing.vhdx")).unwrap(),
            b"backing storage"
        );
    }
}
