//! Plain Axum JSON routes served alongside the Leptos app.
//!
//! The dashboard's server functions pull [`AppState`](crate::state::AppState)
//! out of the Leptos reactive context (`expect_context`), which only exists
//! while SSR-ing a page or serving a server-fn request. These routes are
//! ordinary Axum handlers with typed `State<AppState>` extraction, so external
//! clients get the same scoring without any HTML/session machinery.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};

use crate::dto::{ApiError, IndexToday};
use crate::server::{hours_until_local_midnight, score_for_window};
use crate::state::AppState;

/// `GET /api/index/today` — the "rest of today" index (0–100) for the
/// configured default place: the same scoring as the dashboard's rest-of-day
/// card, over a window from now until local (Oslo) midnight (whole hours,
/// clamped to 1..=24).
///
/// Datoen følger beregningsvinduet, ikke tidspunktet HTTP-svaret sendes.
/// Krysser et tregt værkall midnatt, kan klienten avvise gårsdagens indeks.
///
/// Any scoring failure (MET/Frost unavailable, no default place configured)
/// becomes `503 Service Unavailable` with a generic message; the cause is
/// logged and never echoed to API consumers.
pub async fn index_today(
    State(state): State<AppState>,
) -> Result<Json<IndexToday>, (StatusCode, Json<ApiError>)> {
    let now = Utc::now();
    let date = now.with_timezone(&chrono_tz::Europe::Oslo).date_naive();
    let hours = hours_until_local_midnight(now);
    match score_for_window(&state, String::new(), hours).await {
        Ok(agg) => {
            let produced_at: DateTime<Utc> = agg.produced_at.unwrap_or_else(Utc::now);
            Ok(Json(IndexToday {
                score: agg.mean,
                date,
                produced_at,
            }))
        }
        Err(error) => {
            tracing::warn!(%error, "GET /api/index/today: score unavailable");
            Err((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ApiError {
                    error: "Score er midlertidig utilgjengelig".to_owned(),
                }),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;

    /// Regression guard for the local-midnight window: whole (floor) hours to
    /// the next Oslo midnight, clamped to 1..=24, across DST offsets. Same
    /// arithmetic the dashboard's rest-of-day card has always used.
    #[test]
    fn rest_of_day_window_is_whole_hours_to_midnight_clamped_1_to_24() {
        let cases = [
            // Summer (CEST, UTC+2): 14:20 local → 9 h 40 min to midnight.
            ("2026-06-15T12:20:00Z", 9),
            // 23:31 local → 29 min → floor 0, clamped up to 1.
            ("2026-06-15T21:31:00Z", 1),
            // 00:05 local → 23 h 55 min to the *next* midnight.
            ("2026-06-15T22:05:00Z", 23),
            // Exactly local midnight → a full 24 h window.
            ("2026-06-15T22:00:00Z", 24),
            // Winter (CET, UTC+1): 13:20 local → 10 h 40 min to midnight.
            ("2026-01-15T12:20:00Z", 10),
        ];
        for (instant, expected) in cases {
            let now = DateTime::parse_from_rfc3339(instant)
                .expect("valid rfc3339")
                .with_timezone(&Utc);
            assert_eq!(
                hours_until_local_midnight(now),
                expected,
                "window for {instant}"
            );
        }
    }
}
