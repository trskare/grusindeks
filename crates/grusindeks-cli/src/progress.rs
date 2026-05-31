//! Terminal progress UI for the fetch/score pipeline.
//!
//! Implements [`run::ProgressSink`] on top of `indicatif::MultiProgress`,
//! with two stacked bars:
//!
//! 1. A spinner for the Frost historical-precip lookup (only created
//!    when the `ground_started` event fires — Frost is optional).
//! 2. A counted bar for the parallel locationforecast fan-out.
//!
//! All bars draw to **stderr** so `--json` output on stdout stays clean,
//! and use `enable_steady_tick` so cache hits don't flash a bar that
//! disappears milliseconds later. When stderr isn't a TTY (CI, pipes,
//! file redirection), `indicatif` swaps in a hidden draw target — the
//! sink calls become no-ops without us having to check.
//!
//! Colours follow the same gruvbox accents as the renderer: the spinner
//! is blue, the forecast bar is green when filling and gray when empty.
//! See `theme.rs` for the palette.

use std::sync::Mutex;
use std::time::Duration;

use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};

use crate::run::ProgressSink;

/// `ProgressSink` implementation backed by an `indicatif::MultiProgress`.
/// Bars are created lazily on the matching event and torn down on
/// `finish()` — call that before printing the final renderer output so
/// the terminal cursor lands on a clean line.
pub struct TerminalProgress {
    multi: MultiProgress,
    frost: Mutex<Option<ProgressBar>>,
    forecast: Mutex<Option<ProgressBar>>,
    nowcast: Mutex<Option<ProgressBar>>,
}

impl TerminalProgress {
    pub fn new() -> Self {
        // Draw to stderr so stdout (which carries the rendered Grusindeks
        // or `--json` payload) stays untouched. `indicatif` swaps in a
        // hidden target automatically when stderr isn't a TTY, so this
        // is silent in CI/pipes/file-redirected runs.
        let multi = MultiProgress::with_draw_target(ProgressDrawTarget::stderr());
        Self {
            multi,
            frost: Mutex::new(None),
            forecast: Mutex::new(None),
            nowcast: Mutex::new(None),
        }
    }

    /// Tear down all live bars. Idempotent and cheap. Safe to call even
    /// if no bars were ever created (e.g. cache-only run).
    pub fn finish(&self) {
        self.clear_bars();
    }

    fn clear_bars(&self) {
        if let Some(pb) = self.frost.lock().unwrap().take() {
            pb.finish_and_clear();
        }
        if let Some(pb) = self.forecast.lock().unwrap().take() {
            pb.finish_and_clear();
        }
        if let Some(pb) = self.nowcast.lock().unwrap().take() {
            pb.finish_and_clear();
        }
    }

    fn make_frost(&self) -> ProgressBar {
        let pb = self
            .multi
            .add(ProgressBar::new_spinner().with_style(spinner_style()));
        // 150ms keeps cache hits from drawing at all (the bar is removed
        // before the first tick fires).
        pb.enable_steady_tick(Duration::from_millis(150));
        pb.set_message("Henter historikk fra Frost…");
        pb
    }

    fn make_forecast(&self, total: u64) -> ProgressBar {
        let pb = self
            .multi
            .add(ProgressBar::new(total).with_style(bar_style()));
        pb.enable_steady_tick(Duration::from_millis(150));
        pb.set_message("Henter prognose");
        pb
    }

    fn make_nowcast(&self) -> ProgressBar {
        let pb = self
            .multi
            .add(ProgressBar::new_spinner().with_style(spinner_style()));
        pb.enable_steady_tick(Duration::from_millis(150));
        pb.set_message("Henter radar (Norden)…");
        pb
    }
}

impl Default for TerminalProgress {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TerminalProgress {
    /// Defensive cleanup: if `finish()` wasn't called (panic, early
    /// return, error path), drop still wipes the bars so the terminal
    /// doesn't keep stale artefacts.
    fn drop(&mut self) {
        self.clear_bars();
    }
}

impl ProgressSink for TerminalProgress {
    fn ground_started(&self) {
        let mut slot = self.frost.lock().unwrap();
        if slot.is_none() {
            *slot = Some(self.make_frost());
        }
    }

    fn ground_finished(&self, _found: bool) {
        // Clear (don't leave a final-state line) so the rendered output
        // owns the terminal alone. Previously we called
        // `finish_with_message("✓ Historikk hentet")`, which left the
        // spinner-frame + message visible *next to* the report header
        // — visible as `⠏ ✓ Historikk hentet ...` lekkasje on first run.
        if let Some(pb) = self.frost.lock().unwrap().take() {
            pb.finish_and_clear();
        }
    }

    fn forecast_started(&self, total: usize) {
        let mut slot = self.forecast.lock().unwrap();
        if slot.is_none() {
            *slot = Some(self.make_forecast(total as u64));
        }
    }

    fn forecast_point_done(&self) {
        if let Some(pb) = self.forecast.lock().unwrap().as_ref() {
            pb.inc(1);
        }
    }

    fn forecast_finished(&self) {
        if let Some(pb) = self.forecast.lock().unwrap().take() {
            pb.finish_and_clear();
        }
    }

    fn nowcast_started(&self) {
        let mut slot = self.nowcast.lock().unwrap();
        if slot.is_none() {
            *slot = Some(self.make_nowcast());
        }
    }

    fn nowcast_finished(&self, _found: bool) {
        if let Some(pb) = self.nowcast.lock().unwrap().take() {
            pb.finish_and_clear();
        }
    }
}

/// Spinner template — gruvbox blue dot, dim gray message.
fn spinner_style() -> ProgressStyle {
    ProgressStyle::with_template("  {spinner:.cyan} {msg}")
        .expect("spinner template is valid")
        .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
}

/// Counted-bar template. Width set to 20 cells so it stays narrow
/// enough not to wrap on small terminals.
fn bar_style() -> ProgressStyle {
    ProgressStyle::with_template(
        "  {msg:.bold} [{bar:20.green/white}] {pos}/{len} punkter  {elapsed:.dim}",
    )
    .expect("bar template is valid")
    .progress_chars("█░ ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `TerminalProgress::finish` must be a no-op when no bars were ever
    /// created. Otherwise `--json` runs against a warm cache would print
    /// a phantom line.
    #[test]
    fn finish_is_safe_when_no_bars_created() {
        let p = TerminalProgress::new();
        p.finish(); // must not panic
    }

    /// The sink trait methods must each be safe to call repeatedly.
    /// `finish` should clear bars even mid-flight (defensive — covers
    /// the error path where `run_*` returns Err before
    /// `forecast_finished` would normally fire).
    #[test]
    fn finish_clears_in_flight_bars() {
        let p = TerminalProgress::new();
        p.forecast_started(5);
        p.forecast_point_done();
        p.finish();
    }
}
