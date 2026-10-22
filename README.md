# wslim

Minimal Windows CLI for **restorable WSL distribution backups** without generated project dependencies.

`wslim` exports a stopped source distribution, imports a disposable clone, removes selected generated directories **only in that clone**, and creates a verified `.tar.xz` or `.tar.gz` archive using a second native WSL export. With `--cleanup none`, it skips the clone and cleanup, using the source export directly. The original distro's files are never cleaned or unregistered.

Output naming: `{distro}-backup-yyyy-mm-dd-HHmmss.tar.xz` or `.tar.gz` (local Windows time).

## Requirements

- Windows with WSL installed and at least one registered distribution (WSL 1 or 2).
- Sufficient free disk space for two TAR exports and temporary imported distributions.
- `/bin/sh`, `find` and `rm` in the selected Linux distribution.
- Launch `wslim.exe` from **Windows PowerShell, CMD or Windows Terminal**, not from inside WSL.

## Interactive mode

```powershell
wslim
```

Uses [inquire](https://github.com/mikaelmello/inquire) to select an operation, WSL distribution, cleanup categories and backup settings. The interactive compression menu supports XZ and Gzip, with levels in ascending order: **1 – Fast**, **3 – Standard**, **6 – Balanced (recommended)**, and **9 – Maximum (slow, high memory usage)**. Level 6 is initially selected unless `backup.level` is configured. `--level` accepts levels 0–9, and the old `--xz-level` flag remains an alias. A backup always requires explicit approval before it terminates the source distribution. You can also choose **config** to edit the saved Windows backup directory.

## Non-interactive and scriptable mode

**Every wizard option has a CLI equivalent.** Explicit subcommands never launch the wizard. For backup automation, pass `--yes` to acknowledge terminating the source distro, and `--non-interactive` to prohibit prompts even when running in a terminal.

```powershell
# List distros
wslim list
wslim list --json

# See which generated files would be omitted
wslim preview dev
wslim preview dev --cleanup node,php --json

# Save a default Windows backup destination for all distributions
wslim config backup.output D:\WSLBackups

# Review or change the default interactively
wslim config

# Full backup using configured destination and compression defaults
wslim backup dev

# Change default compression format and level
wslim config set backup.format gz
wslim config set backup.level 3

# Override defaults for a single backup
wslim backup dev --format xz --level 6

# Or override it for this backup
wslim backup dev --output E:\OtherBackups

# Script / scheduled task / future lazygit-wslim frontend
wslim backup dev --output D:\WSLBackups `
  --cleanup node,php,rust,caches `
  --format xz --level 6 --yes --non-interactive --no-progress --json

# Automated restore, never overwrites a registered distro
wslim restore D:\WSLBackups\dev-backup-2026-10-09-145315.tar.xz `
  --as dev-restored --install-dir D:\WSL\dev-restored `
  --version 2 --non-interactive --no-progress --json
```

### Persistent backup destination

The CLI stores per-user settings in `%LOCALAPPDATA%\wslim\config.json` on **Windows**, not inside the selected WSL distro. Supported persistent keys are `backup.output` (an absolute Windows directory path), `backup.format` (`xz` or `gz`), and `backup.level` (`1`, `3`, `6`, or `9`). When unset, compression defaults to **XZ level 6**.

```powershell
wslim config                                  # Interactive settings menu
wslim config list                             # List settings without prompts
wslim config get backup.output                # Read saved path
wslim config set backup.output D:\WSLBackups   # Explicit set syntax
wslim config backup.output D:\WSLBackups       # Short set syntax
wslim config unset backup.output              # Clear saved path
wslim config set backup.format gz             # Default archive type
wslim config set backup.level 3               # Default level (1, 3, 6, 9)
wslim config get backup.format
wslim config get backup.level
wslim config unset backup.format              # Reset to xz
wslim config unset backup.level               # Reset to 6
wslim config list --json                      # Machine-readable settings
```

When changing `backup.output`, the interactive prompt shows the current directory in parentheses. The direct `config set` and `config backup.output` commands print the previous value after saving (for example, `backup.output set to E:\WSLBackups (previous: D:\WSLBackups)`). JSON output remains machine-readable and unchanged.

The backup destination is resolved as **`--output` / `--target` > saved `backup.output` > interactive input**. If no path is configured and prompting is disabled, backup fails with a nonzero exit code.

Before the original WSL distro is stopped, `wslim` creates the destination directory when possible, checks it is writable, and verifies it can create/remove a disposable probe file. If it cannot access the destination (e.g. an external drive is disconnected), interactive mode prints the error and asks for another Windows directory. It does not automatically rewrite your saved preference. In `--non-interactive` or `--json` mode, the same error is reported without prompting, so calling programs can react reliably.

```powershell
# Use the configured path with no interaction (suitable for lazygit-wslim)
wslim backup dev --yes --non-interactive --no-progress --json

# Explicit output always overrides the saved setting
wslim backup dev --output E:\WSLBackups --yes --non-interactive --json
```

### Options

| Option | Applies to | Meaning |
| --- | --- | --- |
| `--output`, `--target`, `-o` | backup | Override the saved destination for this backup |
| `--yes`, `-y` | backup | Approve source termination without prompting |
| `--cleanup node,php,rust,caches` | backup, preview | Select cleanup groups; defaults to all |
| `--cleanup none` | backup, preview | Keep all project dependencies and caches |

For backups, `--cleanup none` also skips temporary stage-distro import and cleanup. The optional test-restore remains independent and can still import a temporary verification distro.
| `--format xz\|gz` | backup | Override compression format; default `backup.format` or xz |
| `--level 0..9` | backup | Override compression level; default `backup.level` or 6 |
| `--xz-level 0..9` | backup | Backwards-compatible alias for `--level` |
| `--no-verify` | backup | Skip disposable test import and shell boot test |
| `--as` | restore | New distro name (required) |
| `--install-dir` | restore | Directory for restored distro (required, must not exist) |
| `--version 1\|2` | restore | WSL version of the new distro; default 2 |
| `--non-interactive` | global | Fail instead of prompting; useful for integrations |
| `--no-progress` | global | Disable spinners and byte progress bars |
| `--json` | global | Emit exactly one JSON result on stdout |

`--json` writes human-readable errors to stderr and returns a nonzero exit code on failure. The successful JSON payloads have stable, simple shapes:

```json
["dev", "Ubuntu"]
```

`list --json` produces an array of names. `preview --json` produces an object such as `{"distro":"dev","paths":["/home/me/app/node_modules"]}`. Backup returns `{"distro":"dev","archive":"D:\\WSLBackups\\dev-backup-....tar.xz","size_bytes":123456}`, and restore returns `{"distro":"dev-restored","install_dir":"D:\\WSL\\dev-restored"}`.

Spinners from [indicatif](https://github.com/console-rs/indicatif) render on stderr only when attached to an interactive terminal. Compression and decompression display byte progress. For archives larger than 16 MiB, compression also **estimates the final archive size** from the bytes already processed and compressed. The display updates at most once per second (and only after at least 20% of the source has been read) without a separate sampling pass. The estimate is approximate: compression ratios can change throughout a TAR stream, and buffered encoder output can delay updates. When an operation finishes, its name and elapsed time remain visible in the terminal, for example `Completed: Exporting cleaned clone (42 s)` or `Completed: Compressing XZ (level 6) (1 h 2 m 5 s)`. Failed operations also retain their name and elapsed time. Progress messages never contaminate `--json` stdout, and are omitted with `--no-progress`.

## Backup lifecycle

1. Stop the selected source distro and export it as a TAR.
2. With cleanup enabled, import that TAR into an isolated temporary distro, preserving WSL version; delete the original TAR after import succeeds.
3. Clean only the temporary clone. The source files remain unchanged.
4. Export the cleaned clone as another TAR, then unregister the stage distro immediately to free its disk. With `--cleanup none`, use the source TAR directly and skip this stage distro.
5. Unless `--no-verify` was provided, test-import the selected TAR, run `/bin/sh`, and unregister the test distro on success.
6. Compress with the selected XZ or Gzip format, verify the complete compressed stream, and publish the final archive.
7. Remove the selected TAR and remaining scratch files on normal completion or recoverable errors.

The tool does not touch Windows-side WSL config, external mounts, or external databases. Its boot test confirms only a basic shell launch, not every service or project. Stop active workloads and separately back up live databases before running an export.

### Cleanup policy

The embedded `src/cleanup.sh` scans `/home`, `/root`, `/workspace`, `/workspaces`, `/srv` and `/opt` without crossing mount boundaries.

| Group | Candidate | Required marker |
| --- | --- | --- |
| `node` | `node_modules` | Sibling `package.json` |
| `php` | `vendor` | Sibling `composer.json` |
| `rust` | `target` | Sibling `Cargo.toml` |
| `caches` | Selected npm, pnpm, Composer and Cargo cache paths | Known user cache path |

`.cargo/bin`, project sources, SSH keys, credentials, and configuration are not intentionally removed. Nevertheless, custom projects may use `vendor`, `target`, or `node_modules` for unique data. **Review `wslim preview` before your first real backup.** Deleted generated dependencies must be reinstalled after restoration.

## Restore

Restore imports a new WSL distribution. It cannot replace an existing registered distro or existing install directory. The tool detects `.tar.xz` or `.tar.gz` from the filename and decompresses to a temporary TAR and imports that TAR with `wsl.exe --import`. A failed partially registered restore is deliberately left for inspection rather than deleted automatically.

An imported distro may default to root unless `/etc/wsl.conf` specifies the intended user. For WSL 1 archives, pass `--version 1`. A full, unfiltered archive is available via `--cleanup none`; even then, files outside the WSL rootfs and live external databases need separate backups.

## Build and release

```powershell
cargo fmt --all -- --check
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
.\target\release\wslim.exe --help
```

Publish the source to [datarose-dev/wslim](https://github.com/datarose-dev/wslim). Pushing a `v*` tag triggers `.github/workflows/release.yml`, which produces `wslim-windows-x86_64.zip` for [marcosnils/bin](https://github.com/marcosnils/bin):

```powershell
bin install github.com/datarose-dev/wslim
```

This project only targets Windows x64. `Cargo.lock` should be generated and committed before publishing a binary release.

## Modules

| File | Responsibility |
| --- | --- |
| `main.rs` | Environment check and command dispatch |
| `cli.rs` | Typed, stable CLI contract via `clap` |
| `config.rs` | Persistent Windows settings and config CLI |
| `output.rs` | Backup destination resolution and writability preflight |
| `interactive.rs` | Optional `inquire` wizard |
| `progress.rs` | `indicatif` progress confined to stderr |
| `backup.rs` and `restore.rs` | Backup and restore workflows |
| `wsl.rs` | WSL calls, distro discovery and clone lifecycle |
| `cleanup.rs` and `cleanup.sh` | Shared cleanup implementation |
| `archive.rs` | XZ I/O and validation |
| `workspace.rs` | Temporary file cleanup |

## Safety and testing

A backup can contain secrets; the `.tar.xz` format **does not encrypt** them. Keep archives on secured storage with an independent copy.

The source is provided without a validated live Windows/WSL integration run. Test against a disposable distro and verify a restoration before entrusting it with irreplaceable data. A forced process kill may leave temporary `wslim-stage-*` / `wslim-test-*` distributions; verify ownership before unregistering any of them.
