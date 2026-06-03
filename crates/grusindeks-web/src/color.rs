//! Score → colour mapping. Mirrors `grusindeks-cli/src/theme.rs::score_color`
//! so the web UI and the CLI agree on which score is which colour.
//!
//! The bucket boundaries here are the single source of truth for the web side;
//! the parity test below pins them to the documented CLI buckets so the two
//! can't silently drift. Class strings are spelled out as literals (not
//! `format!`-ed) so Tailwind's source scanner emits them.

/// Five score buckets, matching `theme.rs` and `score::label_for`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bucket {
    Bad,      // 0..=24   red
    Marginal, // 25..=44  orange
    Ok,       // 45..=64  yellow
    Good,     // 65..=84  lime
    Great,    // 85..=100 green
}

pub fn bucket(total: u8) -> Bucket {
    match total {
        0..=24 => Bucket::Bad,
        25..=44 => Bucket::Marginal,
        45..=64 => Bucket::Ok,
        65..=84 => Bucket::Good,
        _ => Bucket::Great,
    }
}

/// Tailwind text-colour class for a score.
pub fn text_class(total: u8) -> &'static str {
    match bucket(total) {
        Bucket::Bad => "text-gruv-red",
        Bucket::Marginal => "text-gruv-orange",
        Bucket::Ok => "text-gruv-yellow",
        Bucket::Good => "text-gruv-lime",
        Bucket::Great => "text-gruv-green",
    }
}

/// Tailwind background-colour class for a score.
pub fn bg_class(total: u8) -> &'static str {
    match bucket(total) {
        Bucket::Bad => "bg-gruv-red",
        Bucket::Marginal => "bg-gruv-orange",
        Bucket::Ok => "bg-gruv-yellow",
        Bucket::Good => "bg-gruv-lime",
        Bucket::Great => "bg-gruv-green",
    }
}

/// Sub-score bar fill. Bars are monochrome (aqua) by default so colour keeps
/// meaning — only genuinely weak axes turn orange/red. Spelled as literals so
/// Tailwind's source scanner emits them.
///
/// Note: prefer `text-gruv-fg/70` over `text-gruv-gray` for meaningful body
/// text — `gruv-gray` (#928374) on the dark backgrounds is below WCAG AA
/// (~3.4:1 on bg0) and should be reserved for large/bold decorative eyebrows.
pub fn bar_fill_class(val: u8) -> &'static str {
    match val {
        0..=44 => "bg-gruv-red",
        45..=64 => "bg-gruv-orange",
        _ => "bg-gruv-aqua",
    }
}

/// Hex string for the score — used where a literal colour is needed (SVG
/// stroke, MapLibre data-driven styling).
pub fn hex(total: u8) -> &'static str {
    match bucket(total) {
        Bucket::Bad => "#cc241d",
        Bucket::Marginal => "#fe8019",
        Bucket::Ok => "#fabd2f",
        Bucket::Good => "#b8bb26",
        Bucket::Great => "#689d6a",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_match_documented_cli_boundaries() {
        // These are exactly theme.rs::score_color's match arms. If the CLI
        // ever moves a boundary, this test must move with it.
        for (score, expect) in [
            (0u8, Bucket::Bad),
            (24, Bucket::Bad),
            (25, Bucket::Marginal),
            (44, Bucket::Marginal),
            (45, Bucket::Ok),
            (64, Bucket::Ok),
            (65, Bucket::Good),
            (84, Bucket::Good),
            (85, Bucket::Great),
            (100, Bucket::Great),
        ] {
            assert_eq!(bucket(score), expect, "score {score}");
        }
    }
}
