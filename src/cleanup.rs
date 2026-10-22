//! The exact same cleanup policy powers both preview and disposable-clone cleanup.

use crate::{cli::CleanupOptions, wsl, Result};

const SCRIPT: &str = include_str!("cleanup.sh");

pub(crate) enum Mode {
    Preview,
    Clean,
}

pub(crate) fn run(distro: &str, mode: Mode, options: &CleanupOptions) -> Result<Vec<String>> {
    let script = SCRIPT.replace("\r\n", "\n");
    let action = match mode {
        Mode::Preview => "preview",
        Mode::Clean => "clean",
    };
    let stdout = wsl::capture(&[
        "-d",
        distro,
        "-u",
        "root",
        "--",
        "/bin/sh",
        "-c",
        &script,
        "wslim",
        action,
        &options.flags(),
    ])?;
    Ok(paths_from_stdout(&stdout))
}

fn paths_from_stdout(stdout: &str) -> Vec<String> {
    // The shell rejects newline-bearing paths; preserve every other character.
    stdout
        .split_terminator('\n')
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{path::PathBuf, process::Command};

    #[test]
    fn path_protocol_preserves_spaces_tabs_backslashes_and_carriage_returns() {
        let paths = [
            "/home/a space/node_modules",
            "/home/a\t/vendor",
            "/home/a\\/target",
            "/home/cache\r",
        ];
        assert_eq!(paths_from_stdout(&format!("{}\n", paths.join("\n"))), paths);
        assert!(paths_from_stdout("").is_empty());
    }

    fn fixture_shell() -> Option<PathBuf> {
        #[cfg(windows)]
        {
            // Never resolve bash.exe through PATH: Windows' bash.exe launches WSL.
            let mut roots: Vec<PathBuf> = ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"]
                .into_iter()
                .filter_map(std::env::var_os)
                .map(PathBuf::from)
                .collect();
            roots.push(PathBuf::from(r"C:\Program Files"));
            roots
                .into_iter()
                .map(|root| root.join("Git").join("bin").join("bash.exe"))
                .find(|path| path.is_file())
        }
        #[cfg(not(windows))]
        {
            let shell = PathBuf::from("/bin/sh");
            shell.is_file().then_some(shell)
        }
    }

    #[test]
    fn posix_cleanup_fixtures() {
        let Some(shell) = fixture_shell() else {
            eprintln!("Skipping cleanup fixtures: /bin/sh or Git Bash at a standard installation path is required");
            return;
        };
        let mut command = Command::new(shell);
        #[cfg(windows)]
        command.args(["--noprofile", "--norc"]);
        let output = command
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .arg("tests/cleanup-fixtures.sh")
            .output()
            .expect("could not start cleanup fixture shell");
        assert!(
            output.status.success(),
            "cleanup fixtures failed ({}):\n{}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}
