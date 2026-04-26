//! Shared HTTP client for `api.met.no` and `frost.met.no`.
//!
//! Single source of truth for the MET Terms of Service:
//! - identifying `User-Agent` (validated at construction time)
//! - 4-decimal coordinate truncation (handled by `grusindeks_core::geo`)
//! - `Expires` / `Last-Modified` cache revalidation (added in `cache.rs`)
//!
//! `base_url` is injectable so tests can point the client at a local
//! `wiremock` server instead of the real MET endpoints.

use std::time::Duration;

use thiserror::Error;
use url::Url;

/// Errors surfaced by the client. Network/HTTP details are kept opaque so
/// callers don't need to depend on `reqwest` directly.
#[derive(Debug, Error)]
pub enum ClientError {
    #[error("invalid User-Agent: {0}")]
    InvalidUserAgent(&'static str),
    #[error("invalid base URL: {0}")]
    InvalidBaseUrl(String),
    /// A required credential (e.g. the Frost `client_id`) is not configured.
    /// Callers can use this to either prompt the user for setup or fall
    /// back to a degraded code path that doesn't need the credential.
    #[error("missing credentials: {0}")]
    MissingCredentials(&'static str),
    #[error("HTTP error: {status}{}", .body.as_ref().map(|b| format!(" ({b})")).unwrap_or_default())]
    Http { status: u16, body: Option<String> },
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("could not parse response: {0}")]
    Decode(String),
}

/// A MET-TOS-compliant `User-Agent`. Validates at construction so
/// downstream code can trust the value.
///
/// The MET TOS requires the header to identify the application by name
/// and provide a way to reach the operator. We enforce both in
/// [`UserAgent::new`] — empty values, whitespace-only contact, and the
/// known-banned tokens (okhttp, Dalvik, fhttp, Java) are rejected.
#[derive(Debug, Clone)]
pub struct UserAgent(String);

const BANNED_TOKENS: &[&str] = &["okhttp", "Dalvik", "fhttp", "Java"];

impl UserAgent {
    /// `app` is something like `"grusindeks"`; `version` like `"0.1.0"`;
    /// `contact` an email address or URL where the operator can be reached.
    pub fn new(app: &str, version: &str, contact: &str) -> Result<Self, ClientError> {
        if app.trim().is_empty() {
            return Err(ClientError::InvalidUserAgent("app name must not be empty"));
        }
        if version.trim().is_empty() {
            return Err(ClientError::InvalidUserAgent("version must not be empty"));
        }
        let c = contact.trim();
        if c.is_empty() {
            return Err(ClientError::InvalidUserAgent(
                "contact (email or URL) must not be empty",
            ));
        }
        if !(c.contains('@') || c.contains("://") || c.contains('.')) {
            return Err(ClientError::InvalidUserAgent(
                "contact must look like an email or URL",
            ));
        }
        let formatted = format!("{}/{} {}", app.trim(), version.trim(), c);
        for banned in BANNED_TOKENS {
            if formatted.contains(banned) {
                return Err(ClientError::InvalidUserAgent(
                    "uses a banned User-Agent token",
                ));
            }
        }
        Ok(UserAgent(formatted))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Configuration for [`MetClient`].
#[derive(Debug, Clone)]
pub struct MetClientConfig {
    pub user_agent: UserAgent,
    /// Base URL for `api.met.no`. Use a wiremock URL in tests.
    pub api_base: Url,
    /// Base URL for `frost.met.no`.
    pub frost_base: Url,
    /// Frost requires HTTP Basic auth with a `client_id` (password is empty).
    pub frost_client_id: Option<String>,
    pub timeout: Duration,
}

impl MetClientConfig {
    pub const DEFAULT_API_BASE: &'static str = "https://api.met.no/";
    pub const DEFAULT_FROST_BASE: &'static str = "https://frost.met.no/";

    pub fn production(user_agent: UserAgent, frost_client_id: Option<String>) -> Self {
        Self {
            user_agent,
            api_base: Url::parse(Self::DEFAULT_API_BASE).expect("default API base URL is valid"),
            frost_base: Url::parse(Self::DEFAULT_FROST_BASE)
                .expect("default Frost base URL is valid"),
            frost_client_id,
            timeout: Duration::from_secs(15),
        }
    }
}

/// Shared HTTP client. Wraps `reqwest::Client` with the User-Agent baked
/// into default headers and exposes the base URLs to endpoint modules.
#[derive(Debug, Clone)]
pub struct MetClient {
    http: reqwest::Client,
    config: MetClientConfig,
}

impl MetClient {
    pub fn new(config: MetClientConfig) -> Result<Self, ClientError> {
        let http = reqwest::Client::builder()
            .user_agent(config.user_agent.as_str())
            .timeout(config.timeout)
            .build()?;
        Ok(Self { http, config })
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    pub fn config(&self) -> &MetClientConfig {
        &self.config
    }

    /// Build an absolute URL relative to `api_base`.
    pub fn api_url(&self, path: &str) -> Result<Url, ClientError> {
        self.config
            .api_base
            .join(path.trim_start_matches('/'))
            .map_err(|e| ClientError::InvalidBaseUrl(e.to_string()))
    }

    /// Build an absolute URL relative to `frost_base`.
    pub fn frost_url(&self, path: &str) -> Result<Url, ClientError> {
        self.config
            .frost_base
            .join(path.trim_start_matches('/'))
            .map_err(|e| ClientError::InvalidBaseUrl(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn ua() -> UserAgent {
        UserAgent::new("grusindeks", "0.1.0", "dev@example.invalid").unwrap()
    }

    // ---- UserAgent validation ----

    #[test]
    fn user_agent_format() {
        assert_eq!(ua().as_str(), "grusindeks/0.1.0 dev@example.invalid");
    }

    #[test]
    fn user_agent_accepts_url_contact() {
        let u = UserAgent::new("grusindeks", "0.1.0", "https://example.org/grusindeks").unwrap();
        assert!(u.as_str().ends_with("https://example.org/grusindeks"));
    }

    #[test]
    fn user_agent_rejects_empty_app() {
        assert!(UserAgent::new("", "0.1.0", "dev@example.invalid").is_err());
        assert!(UserAgent::new("   ", "0.1.0", "dev@example.invalid").is_err());
    }

    #[test]
    fn user_agent_rejects_empty_version() {
        assert!(UserAgent::new("grusindeks", "", "dev@example.invalid").is_err());
    }

    #[test]
    fn user_agent_rejects_empty_contact() {
        assert!(UserAgent::new("grusindeks", "0.1.0", "").is_err());
        assert!(UserAgent::new("grusindeks", "0.1.0", "   ").is_err());
    }

    #[test]
    fn user_agent_rejects_non_contact_string() {
        // No @, no ://, no . — looks like a random token, reject.
        assert!(UserAgent::new("grusindeks", "0.1.0", "anonymous").is_err());
    }

    #[test]
    fn user_agent_rejects_banned_tokens() {
        assert!(UserAgent::new("okhttp", "1.0", "x@y.io").is_err());
        assert!(UserAgent::new("grusindeks", "0.1.0", "Java@runtime.com").is_err());
    }

    // ---- MetClient: base URL ----

    #[test]
    fn api_url_joins_relative_path() {
        let cfg = MetClientConfig::production(ua(), None);
        let client = MetClient::new(cfg).unwrap();
        let u = client
            .api_url("/weatherapi/locationforecast/2.0/complete")
            .unwrap();
        assert_eq!(
            u.as_str(),
            "https://api.met.no/weatherapi/locationforecast/2.0/complete"
        );
    }

    #[test]
    fn api_url_handles_path_without_leading_slash() {
        let cfg = MetClientConfig::production(ua(), None);
        let client = MetClient::new(cfg).unwrap();
        let u = client
            .api_url("weatherapi/locationforecast/2.0/complete")
            .unwrap();
        assert_eq!(
            u.as_str(),
            "https://api.met.no/weatherapi/locationforecast/2.0/complete"
        );
    }

    #[test]
    fn frost_url_uses_frost_base() {
        let cfg = MetClientConfig::production(ua(), None);
        let client = MetClient::new(cfg).unwrap();
        let u = client.frost_url("/observations/v0.jsonld").unwrap();
        assert_eq!(u.as_str(), "https://frost.met.no/observations/v0.jsonld");
    }

    // ---- MetClient: User-Agent actually sent over the wire ----

    #[tokio::test]
    async fn client_sends_user_agent_header() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/ping"))
            .and(header("user-agent", "grusindeks/0.1.0 dev@example.invalid"))
            .respond_with(ResponseTemplate::new(200).set_body_string("pong"))
            .mount(&server)
            .await;

        let cfg = MetClientConfig {
            user_agent: ua(),
            api_base: Url::parse(&format!("{}/", server.uri())).unwrap(),
            frost_base: Url::parse(&format!("{}/", server.uri())).unwrap(),
            frost_client_id: None,
            timeout: Duration::from_secs(5),
        };
        let client = MetClient::new(cfg).unwrap();

        let url = client.api_url("/ping").unwrap();
        let resp = client.http().get(url).send().await.unwrap();
        assert_eq!(resp.status(), 200);
        // If the UA didn't match, wiremock would have returned 404 here.
    }

    #[tokio::test]
    async fn client_uses_injected_base_url() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/weatherapi/locationforecast/2.0/complete"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;

        let cfg = MetClientConfig {
            user_agent: ua(),
            api_base: Url::parse(&format!("{}/", server.uri())).unwrap(),
            frost_base: Url::parse(&format!("{}/", server.uri())).unwrap(),
            frost_client_id: None,
            timeout: Duration::from_secs(5),
        };
        let client = MetClient::new(cfg).unwrap();
        let url = client
            .api_url("/weatherapi/locationforecast/2.0/complete")
            .unwrap();
        let resp = client.http().get(url).send().await.unwrap();
        assert_eq!(resp.status(), 200);
    }
}
