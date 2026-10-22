//! Remove temporary TAR files even when a workflow returns early with an error.

use crate::Result;
use std::{fs, path::{Path, PathBuf}};

pub(crate) struct Workspace {
    pub(crate) path: PathBuf,
}

impl Workspace {
    pub(crate) fn new(parent: &Path, id: &str) -> Result<Self> {
        let path = parent.join(format!(".wslim-work-{id}"));
        fs::create_dir(&path)?;
        Ok(Self { path })
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.path) {
            eprintln!(
                "warning: temporary files remain in {}: {error}",
                self.path.display()
            );
        }
    }
}
