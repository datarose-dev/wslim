//! The exact same cleanup policy powers both preview and disposable-clone cleanup.

use crate::{cli::CleanupOptions, wsl, Result};

const SCRIPT: &str = include_str!("cleanup.sh");

pub(crate) enum Mode {
    Preview,
    Clean,
}

pub(crate) fn run(distro: &str, mode: Mode, options: &CleanupOptions) -> Result<Vec<String>> {
    let script = SCRIPT.replace("\r\n", "\n");
    let action = match mode { Mode::Preview => "preview", Mode::Clean => "clean" };
    let stdout = wsl::capture(&[
        "-d", distro, "-u", "root", "--", "/bin/sh", "-c", &script,
        "wslim", action, &options.flags(),
    ])?;
    Ok(stdout.lines().filter(|line| !line.trim().is_empty()).map(str::to_owned).collect())
}
