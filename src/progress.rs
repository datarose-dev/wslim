//! Terminal-only progress indicators; JSON output remains machine-readable.

use indicatif::{ProgressBar, ProgressStyle};
use std::{
    io::{self, IsTerminal},
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
pub(crate) struct Progress {
    enabled: bool,
}

impl Progress {
    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn new(no_progress: bool) -> Self {
        Self {
            enabled: !no_progress && io::stderr().is_terminal(),
        }
    }

    pub(crate) fn step<T, E>(
        &self,
        label: &str,
        work: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        let bar = if self.enabled {
            ProgressBar::new_spinner()
        } else {
            ProgressBar::hidden()
        };
        if self.enabled {
            bar.set_style(
                ProgressStyle::with_template("{spinner} {msg} [{elapsed_precise}]")
                    .expect("static progress template"),
            );
            bar.set_message(label.to_owned());
            bar.enable_steady_tick(Duration::from_millis(100));
        }

        let started = Instant::now();
        let result = work();
        if result.is_ok() {
            self.complete(&bar, label, started.elapsed());
        } else {
            self.fail(&bar, label, started.elapsed());
        }
        result
    }

    pub(crate) fn bytes(&self, count: u64, label: &str) -> ProgressBar {
        if !self.enabled {
            return ProgressBar::hidden();
        }
        let bar = ProgressBar::new(count);
        bar.set_style(
            ProgressStyle::with_template("{spinner} {msg} [{bar:30}] {bytes}/{total_bytes}")
                .expect("static progress template"),
        );
        bar.set_message(label.to_owned());
        bar
    }

    /// Keep the operation name and elapsed time visible after completion.
    pub(crate) fn complete(&self, bar: &ProgressBar, label: &str, elapsed: Duration) {
        self.finish(bar, label, elapsed, true);
    }

    pub(crate) fn fail(&self, bar: &ProgressBar, label: &str, elapsed: Duration) {
        self.finish(bar, label, elapsed, false);
    }

    fn finish(&self, bar: &ProgressBar, label: &str, elapsed: Duration, success: bool) {
        if !self.enabled {
            return;
        }

        // Replace the live indicator with a colored status icon, not an elapsed timer.
        let template = if success { "{prefix:.green} {msg}" } else { "{prefix:.red} {msg}" };
        bar.set_style(ProgressStyle::with_template(template).expect("static progress template"));
        bar.set_prefix(if success { "✓" } else { "✗" });
        let message = format!("{label} ({})", human_duration(elapsed));
        if success {
            bar.finish_with_message(message);
        } else {
            bar.abandon_with_message(message);
        }
    }
}

fn human_duration(duration: Duration) -> String {
    let total = duration.as_secs();
    if total == 0 {
        return "<1 s".to_owned();
    }
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;
    if hours > 0 {
        format!("{hours} h {minutes} m {seconds} s")
    } else if minutes > 0 {
        format!("{minutes} m {seconds} s")
    } else {
        format!("{seconds} s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_readable_elapsed_time() {
        assert_eq!(human_duration(Duration::from_millis(500)), "<1 s");
        assert_eq!(human_duration(Duration::from_secs(8)), "8 s");
        assert_eq!(human_duration(Duration::from_secs(125)), "2 m 5 s");
        assert_eq!(human_duration(Duration::from_secs(3725)), "1 h 2 m 5 s");
    }
}
