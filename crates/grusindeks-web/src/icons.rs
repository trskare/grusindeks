//! Monochrome inline-SVG icons (Lucide-style geometry). Size and colour are set
//! by the caller via a Tailwind `class` on the `<svg>`; colour follows
//! `currentColor`, so a `text-gruv-*` class on the icon (or an ancestor) tints
//! it. Each icon is a plain free function — no component overhead — matching the
//! helper style in [`crate::components`].
//!
//! Pass `Some(label)` when the icon stands in for text (it gets an accessible
//! name via `<title>` + `role="img"`); pass `None` for decorative icons that sit
//! next to a visible text label (they're hidden from assistive tech).
//!
//! Size scale (keep call sites on these rungs so icon rows read as intentional):
//! `h-[18px]` lane gutters · `h-4` stat tiles / timeline lane keys · `h-3.5`
//! inline chips · `h-3` legends & decorative. Stroke stays `2` at every size.

use leptos::prelude::*;

/// Shared `<svg>` shell. `body` holds the icon's geometry.
fn base(class: &'static str, title: Option<&'static str>, body: AnyView) -> impl IntoView {
    view! {
        <svg
            class=class
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
            role=title.map(|_| "img")
            aria-hidden=title.is_none().then_some("true")
        >
            {title.map(|t| view! { <title>{t}</title> })}
            {body}
        </svg>
    }
}

/// Thermometer — Temperatur.
pub fn thermometer(class: &'static str, title: Option<&'static str>) -> impl IntoView {
    base(
        class,
        title,
        view! { <path d="M14 4v10.54a4 4 0 1 1-4 0V4a2 2 0 0 1 4 0Z"/> }.into_any(),
    )
}

/// Wind — Vind.
pub fn wind(class: &'static str, title: Option<&'static str>) -> impl IntoView {
    base(
        class,
        title,
        view! {
            <path d="M12.8 19.6A2 2 0 1 0 14 16H2"/>
            <path d="M17.5 8a2.5 2.5 0 1 1 1.79 4.25H2"/>
            <path d="M9.8 4.4A2 2 0 1 1 11 8H2"/>
        }
        .into_any(),
    )
}

/// Cloud with rain — Nedbør (amount).
pub fn cloud_rain(class: &'static str, title: Option<&'static str>) -> impl IntoView {
    base(
        class,
        title,
        view! {
            <line x1="16" x2="16" y1="13" y2="21"/>
            <line x1="8" x2="8" y1="13" y2="21"/>
            <line x1="12" x2="12" y1="15" y2="23"/>
            <path d="M20 16.58A5 5 0 0 0 18 7h-1.26A8 8 0 1 0 4 15.25"/>
        }
        .into_any(),
    )
}

/// Umbrella — Sjanse (probability of precipitation).
pub fn umbrella(class: &'static str, title: Option<&'static str>) -> impl IntoView {
    base(
        class,
        title,
        view! {
            <path d="M22 12a10.06 10.06 0 0 0-20 0Z"/>
            <path d="M12 12v8a2 2 0 0 0 4 0"/>
            <path d="M12 2v1"/>
        }
        .into_any(),
    )
}

/// Mountain / terrain — Bakke (surface).
pub fn mountain(class: &'static str, title: Option<&'static str>) -> impl IntoView {
    base(
        class,
        title,
        view! { <path d="m8 3 4 8 5-5 5 15H2L8 3z"/> }.into_any(),
    )
}

/// Sun over the horizon — solnedgang.
pub fn sunset(class: &'static str, title: Option<&'static str>) -> impl IntoView {
    base(
        class,
        title,
        view! {
            <path d="M12 10V2"/>
            <path d="m4.93 10.93 1.41 1.41"/>
            <path d="M2 18h2"/>
            <path d="M20 18h2"/>
            <path d="m19.07 10.93-1.41 1.41"/>
            <path d="M22 22H2"/>
            <path d="m16 6-4 4-4-4"/>
            <path d="M16 18a4 4 0 0 0-8 0"/>
        }
        .into_any(),
    )
}

/// Single raindrop — over a rain band.
pub fn droplet(class: &'static str, title: Option<&'static str>) -> impl IntoView {
    base(
        class,
        title,
        view! {
            <path d="M12 22a7 7 0 0 0 7-7c0-2-1-3.9-3-5.5s-3.5-4-4-6.5c-.5 2.5-2 4.9-4 6.5C7 11.1 6 13 6 15a7 7 0 0 0 7 7z"/>
        }
        .into_any(),
    )
}

/// Bicycle — the best riding window.
pub fn bike(class: &'static str, title: Option<&'static str>) -> impl IntoView {
    base(
        class,
        title,
        view! {
            <circle cx="18.5" cy="17.5" r="3.5"/>
            <circle cx="5.5" cy="17.5" r="3.5"/>
            <circle cx="15" cy="5" r="1"/>
            <path d="M12 17.5V14l-3-3 4-3 2 3h2"/>
        }
        .into_any(),
    )
}

/// Clock — the recommendation card's "neste 3 timer" horizon badge.
pub fn clock(class: &'static str, title: Option<&'static str>) -> impl IntoView {
    base(
        class,
        title,
        view! {
            <circle cx="12" cy="12" r="10"/>
            <path d="M12 6v6l4 2"/>
        }
        .into_any(),
    )
}

/// Arrow pointing right — the "i morgen" (next day) chip.
pub fn arrow_right(class: &'static str, title: Option<&'static str>) -> impl IntoView {
    base(
        class,
        title,
        view! {
            <path d="M5 12h14"/>
            <path d="m12 5 7 7-7 7"/>
        }
        .into_any(),
    )
}
