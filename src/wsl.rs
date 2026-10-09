//! All WSL process calls and disposable distro lifecycle management.

use crate::Result;
use std::{path::Path, process::Command};

pub(crate) fn run(args: &[&str]) -> Result<()> {
    capture(args)?;
    Ok(())
}

pub(crate) fn capture(args: &[&str]) -> Result<String> {
    let output = Command::new("wsl.exe").args(args).output()?;
    if !output.status.success() {
        let stderr = decode_output(&output.stderr);
        let stdout = decode_output(&output.stdout);
        let details = if stderr.trim().is_empty() { stdout.trim() } else { stderr.trim() };
        return Err(format!("wsl.exe {} failed: {} {details}", args.first().unwrap_or(&""),
            output.status).into());
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
    let output = Command::new("wsl.exe").args(["--list", "--quiet"]).output()?;
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
    if installed_distros()?.iter().any(|distro| distro.eq_ignore_ascii_case(name)) {
        return Err(format!("WSL distro already exists: {name}").into());
    }
    Ok(())
}

pub(crate) fn distro_version(name: &str) -> Result<u8> {
    let output = Command::new("wsl.exe").args(["--list", "--verbose"]).output()?;
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
    if bytes.starts_with(&[0xff, 0xfe]) || bytes.chunks_exact(2).take(64).any(|pair| pair[1] == 0) {
        let units = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
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

/// Tracks only imported temporary distros, never the source or user restores.
#[derive(Default)]
pub(crate) struct TemporaryDistros {
    names: Vec<String>,
}

impl TemporaryDistros {
    pub(crate) fn import(&mut self, name: &str, dir: &Path, tar: &Path, version: u8) -> Result<()> {
        let result = import(name, dir, tar, version);

        // An unsuccessful import can still register a partially created distro.
        let registered = result.is_ok()
            || installed_distros()?.iter().any(|distro| distro.eq_ignore_ascii_case(name));
        if registered {
            self.names.push(name.to_owned());
        }
        result
    }

    pub(crate) fn unregister(&mut self, name: &str) -> Result<()> {
        if self.names.iter().any(|existing| existing == name) {
            run(&["--terminate", name])?;
            run(&["--unregister", name])?;
            self.names.retain(|existing| existing != name);
        }
        Ok(())
    }
}

impl Drop for TemporaryDistros {
    fn drop(&mut self) {
        // A failed backup must not leave gigabyte-sized imported clones behind.
        for name in self.names.drain(..).rev() {
            let _ = Command::new("wsl.exe").args(["--terminate", &name]).status();
            let status = Command::new("wsl.exe").args(["--unregister", &name]).status();
            if !status.is_ok_and(|result| result.success()) {
                eprintln!("warning: unregister temporary distro manually: {name}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let listing = "  NAME       STATE     VERSION\n* dev-old    Stopped   1\n  dev        Running   2\n";
        assert_eq!(version_from_listing(listing, "dev"), Some(2));
    }
}
