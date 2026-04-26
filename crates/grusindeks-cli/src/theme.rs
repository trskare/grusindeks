//! Gruvbox dark palette and small styling helpers for the human renderer.
//!
//! Colours map to score buckets (the same buckets `score::label_for` uses)
//! and to penalty severity. Everything goes through `owo-colors`'
//! `if_supports_color`, so output stays plain ASCII when piped to a file or
//! when `NO_COLOR` is set.

use grusindeks_core::daily::Confidence;
use grusindeks_core::score::{Component, Severity};
use owo_colors::{OwoColorize, Rgb, Stream::Stdout, Style};

// ---- Palette ----
// Gruvbox dark hard-coded as RGB. We don't paint the background — the user's
// terminal already has one — so `bg0` isn't represented here.

pub const FG: Rgb = Rgb(0xeb, 0xdb, 0xb2);
pub const GRAY: Rgb = Rgb(0x92, 0x83, 0x74);
pub const RED: Rgb = Rgb(0xfb, 0x49, 0x34);
pub const ORANGE: Rgb = Rgb(0xfe, 0x80, 0x19);
pub const YELLOW: Rgb = Rgb(0xfa, 0xbd, 0x2f);
pub const GREEN: Rgb = Rgb(0xb8, 0xbb, 0x26);
pub const AQUA: Rgb = Rgb(0x8e, 0xc0, 0x7c);
pub const BLUE: Rgb = Rgb(0x83, 0xa5, 0x98);
pub const PURPLE: Rgb = Rgb(0xd3, 0x86, 0x9b);

/// Color a 0–100 score by the same bucket boundaries `score::label_for`
/// uses, so the colour and the label always agree.
pub fn score_color(total: u8) -> Rgb {
    match total {
        0..=24 => RED,       // Dårlig
        25..=44 => ORANGE,   // Marginalt
        45..=64 => YELLOW,   // OK
        65..=84 => GREEN,    // Bra
        _ => AQUA,           // Strålende
    }
}

/// Color a `Severity` so a list of penalties reads like a triage list.
pub fn severity_color(s: Severity) -> Rgb {
    match s {
        Severity::Minor => YELLOW,
        Severity::Major => ORANGE,
        Severity::Critical => RED,
    }
}

/// Per-component accent — only used for the small label prefix on a
/// penalty line ("Vind:", "Nedbør:", …) so the eye can scan by component.
pub fn component_color(c: Component) -> Rgb {
    match c {
        Component::Temperature => YELLOW,
        Component::Wind => ORANGE,
        Component::Precipitation => BLUE,
        Component::PrecipProbability => BLUE,
        Component::Ground => PURPLE,
        Component::HardCap => RED,
    }
}

/// Norwegian label prefix for a component, used on penalty rows.
pub fn component_label_no(c: Component) -> &'static str {
    match c {
        Component::Temperature => "Temperatur",
        Component::Wind => "Vind",
        Component::Precipitation => "Nedbør",
        Component::PrecipProbability => "Sannsynlighet",
        Component::Ground => "Bakke",
        Component::HardCap => "Advarsel",
    }
}

/// Long-range days have low confidence — render them dimmer so the eye
/// naturally lands on the trustworthy near-term days first.
pub fn dim_for_confidence(confidence: Confidence) -> bool {
    matches!(confidence, Confidence::Lav)
}

// ---- Style helpers ----
//
// All of these go through `if_supports_color(Stdout, ...)` so the styling
// is a no-op when stdout isn't a TTY or `NO_COLOR` is set.

pub fn paint_score(total: u8) -> String {
    let style = Style::new().color(score_color(total)).bold();
    format!("{}", total.if_supports_color(Stdout, |t| t.style(style)))
}

/// Like `paint_score`, but takes a pre-formatted string so the caller can
/// right-align the number (`"{:>3}"`) before painting.
pub fn paint_score_str(s: &str, total: u8) -> String {
    let style = Style::new().color(score_color(total)).bold();
    format!("{}", s.if_supports_color(Stdout, |x| x.style(style)))
}

pub fn paint_label(label: &str, total: u8) -> String {
    let style = Style::new().color(score_color(total));
    format!("{}", label.if_supports_color(Stdout, |s| s.style(style)))
}

pub fn paint_bar_filled(s: &str, total: u8) -> String {
    let style = Style::new().color(score_color(total));
    format!("{}", s.if_supports_color(Stdout, |x| x.style(style)))
}

pub fn paint_bar_empty(s: &str) -> String {
    let style = Style::new().color(GRAY);
    format!("{}", s.if_supports_color(Stdout, |x| x.style(style)))
}

pub fn paint_dim(s: &str) -> String {
    let style = Style::new().color(GRAY);
    format!("{}", s.if_supports_color(Stdout, |x| x.style(style)))
}

pub fn paint_fg(s: &str) -> String {
    let style = Style::new().color(FG);
    format!("{}", s.if_supports_color(Stdout, |x| x.style(style)))
}

pub fn paint_accent(s: &str) -> String {
    let style = Style::new().color(PURPLE).bold();
    format!("{}", s.if_supports_color(Stdout, |x| x.style(style)))
}

pub fn paint_severity(s: &str, sev: Severity) -> String {
    let style = Style::new().color(severity_color(sev));
    format!("{}", s.if_supports_color(Stdout, |x| x.style(style)))
}

pub fn paint_component_label(c: Component) -> String {
    let label = component_label_no(c);
    let style = Style::new().color(component_color(c)).bold();
    format!("{}", label.if_supports_color(Stdout, |x| x.style(style)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_color_matches_label_buckets() {
        assert_eq!(score_color(0), RED);
        assert_eq!(score_color(24), RED);
        assert_eq!(score_color(25), ORANGE);
        assert_eq!(score_color(44), ORANGE);
        assert_eq!(score_color(45), YELLOW);
        assert_eq!(score_color(64), YELLOW);
        assert_eq!(score_color(65), GREEN);
        assert_eq!(score_color(84), GREEN);
        assert_eq!(score_color(85), AQUA);
        assert_eq!(score_color(100), AQUA);
    }

    #[test]
    fn severity_color_orders_by_intensity() {
        // The exact RGBs are an implementation detail; what we care about
        // is that the three are distinct so the eye can tell them apart.
        assert_ne!(severity_color(Severity::Minor), severity_color(Severity::Major));
        assert_ne!(severity_color(Severity::Major), severity_color(Severity::Critical));
    }

    #[test]
    fn dim_for_confidence_only_dims_lav() {
        assert!(!dim_for_confidence(Confidence::Hoy));
        assert!(!dim_for_confidence(Confidence::Middels));
        assert!(dim_for_confidence(Confidence::Lav));
    }
}
