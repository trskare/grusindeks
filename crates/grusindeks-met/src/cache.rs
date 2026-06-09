//! Disk cache that honours `Expires` and revalidates with
//! `If-Modified-Since` / `304 Not Modified` — required by the MET TOS.
//!
//! Storage shape: one JSON file per URL (filename = SHA-256 of the URL),
//! each holding the response body plus the `Last-Modified` and `Expires`
//! values from the original response.

#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::fs;
use url::Url;

use crate::client::ClientError;

/// Disk-backed HTTP cache.
#[derive(Debug, Clone)]
pub struct Cache {
    dir: PathBuf,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct CacheEntry {
    /// `Last-Modified` from the upstream response, verbatim. Echoed back as
    /// `If-Modified-Since` on revalidation.
    last_modified: Option<String>,
    /// Wall-clock time after which the cache must revalidate. Stored as
    /// RFC 3339 UTC.
    expires: Option<DateTime<Utc>>,
    body: String,
}

/// Outcome of a cache lookup, useful for tests and the `--verbose` log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheOutcome {
    /// Returned without any HTTP call.
    Hit,
    /// Sent `If-Modified-Since`, server replied `304 Not Modified`.
    Revalidated,
    /// New body fetched from upstream (no prior entry, or upstream sent 200).
    Refreshed,
    /// Revalidation failed with a transient error (network, 429, 5xx) and the
    /// stale cached body was served instead. Only happens within
    /// [`STALE_GRACE_HOURS`] of the entry's `Expires`.
    StaleOnError,
}

/// How long past `Expires` a cached body may still be served when
/// revalidation fails with a transient error. MET forecasts expire roughly
/// half an hour after fetch, so this tolerates an outage of a working day
/// without ever presenting truly outdated data as current.
const STALE_GRACE_HOURS: i64 = 6;

impl Cache {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn path_for(&self, url: &Url) -> PathBuf {
        let mut h = Sha256::new();
        h.update(url.as_str().as_bytes());
        let digest = hex::encode(h.finalize());
        self.dir.join(format!("{digest}.json"))
    }

    async fn read_entry(&self, url: &Url) -> Option<CacheEntry> {
        let p = self.path_for(url);
        match fs::read(&p).await {
            Ok(bytes) => serde_json::from_slice(&bytes).ok(),
            Err(_) => None,
        }
    }

    async fn write_entry(&self, url: &Url, entry: &CacheEntry) -> Result<(), ClientError> {
        if !self.dir.exists() {
            fs::create_dir_all(&self.dir)
                .await
                .map_err(|e| ClientError::Cache(format!("mkdir: {e}")))?;
        }
        let bytes =
            serde_json::to_vec(entry).map_err(|e| ClientError::Cache(format!("encode: {e}")))?;
        fs::write(self.path_for(url), bytes)
            .await
            .map_err(|e| ClientError::Cache(format!("write: {e}")))?;
        Ok(())
    }

    /// Get the body for `url`, using the cache when fresh and revalidating
    /// with `If-Modified-Since` once `Expires` has passed.
    ///
    /// When revalidation fails *transiently* — the request never reaches the
    /// server, or MET answers 429/5xx — a stale entry within
    /// [`STALE_GRACE_HOURS`] of its `Expires` is served instead of failing,
    /// so a brief MET outage degrades to a slightly old forecast rather than
    /// an empty app. Non-transient errors (403 banned User-Agent, 404, …)
    /// always propagate: those need the operator's attention.
    pub async fn get_or_revalidate(
        &self,
        http: &reqwest::Client,
        url: Url,
    ) -> Result<(String, CacheOutcome), ClientError> {
        let now: DateTime<Utc> = SystemTime::now().into();

        let cached = self.read_entry(&url).await;

        // Fresh hit: return without touching the network.
        if let Some(ref entry) = cached {
            if let Some(exp) = entry.expires {
                if now < exp {
                    return Ok((entry.body.clone(), CacheOutcome::Hit));
                }
            }
        }

        // Otherwise: GET (with If-Modified-Since when we have a value).
        let mut req = http.get(url.clone());
        if let Some(ref entry) = cached {
            if let Some(ref lm) = entry.last_modified {
                req = req.header("if-modified-since", lm);
            }
        }
        let resp = match req.send().await {
            Ok(resp) => resp,
            Err(e) => {
                if let Some(entry) = cached.filter(|e| within_stale_grace(e, now)) {
                    tracing::warn!(%url, error = %e, "MET unreachable — serving stale cache");
                    return Ok((entry.body, CacheOutcome::StaleOnError));
                }
                return Err(e.into());
            }
        };

        // 304: keep the body, refresh Expires from the new headers.
        if resp.status().as_u16() == 304 {
            // We only ever emit If-Modified-Since when we already had a
            // cached entry, so a server that respects RFC 7232 cannot
            // reach this branch without one. A misbehaving upstream that
            // returns 304 unprompted shouldn't crash the client — surface
            // it as a Decode error and let the caller retry.
            let mut entry = cached.ok_or_else(|| {
                ClientError::Decode(
                    "upstream returned 304 Not Modified without a prior cached entry".into(),
                )
            })?;
            entry.expires = parse_expires(resp.headers().get("expires"));
            // Last-Modified usually unchanged on 304, but refresh if echoed.
            if let Some(lm) = resp.headers().get("last-modified") {
                if let Ok(s) = lm.to_str() {
                    entry.last_modified = Some(s.to_owned());
                }
            }
            self.write_entry(&url, &entry).await?;
            return Ok((entry.body, CacheOutcome::Revalidated));
        }

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let transient = status == 429 || resp.status().is_server_error();
            if transient {
                if let Some(entry) = cached.filter(|e| within_stale_grace(e, now)) {
                    tracing::warn!(%url, status, "MET error — serving stale cache");
                    return Ok((entry.body, CacheOutcome::StaleOnError));
                }
            }
            let body = resp.text().await.ok();
            return Err(ClientError::Http { status, body });
        }

        let last_modified = resp
            .headers()
            .get("last-modified")
            .and_then(|h| h.to_str().ok())
            .map(str::to_owned);
        let expires = parse_expires(resp.headers().get("expires"));
        let body = resp.text().await?;

        let entry = CacheEntry {
            last_modified,
            expires,
            body: body.clone(),
        };
        self.write_entry(&url, &entry).await?;
        Ok((body, CacheOutcome::Refreshed))
    }
}

/// Whether a stale entry is still recent enough to serve when revalidation
/// fails transiently. Entries without `Expires` have no age bound we can
/// reason about, so they never qualify.
fn within_stale_grace(entry: &CacheEntry, now: DateTime<Utc>) -> bool {
    entry
        .expires
        .is_some_and(|exp| now < exp + chrono::Duration::hours(STALE_GRACE_HOURS))
}

fn parse_expires(h: Option<&reqwest::header::HeaderValue>) -> Option<DateTime<Utc>> {
    let s = h?.to_str().ok()?;
    DateTime::parse_from_rfc2822(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

/// Check whether a path is inside the cache directory (used by tests).
#[cfg(test)]
pub(crate) fn entry_exists(dir: &Path, url: &Url) -> bool {
    let mut h = Sha256::new();
    h.update(url.as_str().as_bytes());
    dir.join(format!("{}.json", hex::encode(h.finalize())))
        .exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use tempfile::TempDir;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    /// Fmt RFC 1123 (which RFC 7231 uses for Expires/Last-Modified), with
    /// the obsolete `GMT` suffix that real MET responses use.
    fn http_date(t: DateTime<Utc>) -> String {
        t.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
    }

    /// Wiremock responder that counts how many requests it has handled and
    /// honours `If-Modified-Since` by returning 304 when the value matches.
    struct CountingResponder {
        body: String,
        last_modified: String,
        expires: DateTime<Utc>,
        count: Arc<AtomicU64>,
    }

    impl Respond for CountingResponder {
        fn respond(&self, req: &Request) -> ResponseTemplate {
            self.count.fetch_add(1, Ordering::SeqCst);
            let ims = req
                .headers
                .get("if-modified-since")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if ims == self.last_modified {
                return ResponseTemplate::new(304)
                    .insert_header("last-modified", self.last_modified.as_str())
                    .insert_header("expires", http_date(self.expires).as_str());
            }
            ResponseTemplate::new(200)
                .set_body_string(&self.body)
                .insert_header("last-modified", self.last_modified.as_str())
                .insert_header("expires", http_date(self.expires).as_str())
        }
    }

    async fn build_cached_url(server: &MockServer) -> (Cache, TempDir, reqwest::Client, Url) {
        let dir = TempDir::new().unwrap();
        let cache = Cache::new(dir.path().to_path_buf());
        let http = reqwest::Client::builder().build().unwrap();
        let url = Url::parse(&format!("{}/cached", server.uri())).unwrap();
        (cache, dir, http, url)
    }

    #[tokio::test]
    async fn first_call_refreshes_and_writes_entry() {
        let server = MockServer::start().await;
        let count = Arc::new(AtomicU64::new(0));
        Mock::given(method("GET"))
            .and(path("/cached"))
            .respond_with(CountingResponder {
                body: "hello".into(),
                last_modified: http_date(Utc::now() - Duration::minutes(10)),
                expires: Utc::now() + Duration::minutes(30),
                count: count.clone(),
            })
            .mount(&server)
            .await;

        let (cache, _dir, http, url) = build_cached_url(&server).await;
        let (body, outcome) = cache.get_or_revalidate(&http, url.clone()).await.unwrap();
        assert_eq!(body, "hello");
        assert_eq!(outcome, CacheOutcome::Refreshed);
        assert!(entry_exists(_dir.path(), &url));
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn second_call_within_expires_is_a_hit_no_network() {
        let server = MockServer::start().await;
        let count = Arc::new(AtomicU64::new(0));
        Mock::given(method("GET"))
            .and(path("/cached"))
            .respond_with(CountingResponder {
                body: "hello".into(),
                last_modified: http_date(Utc::now() - Duration::minutes(10)),
                expires: Utc::now() + Duration::minutes(30),
                count: count.clone(),
            })
            .mount(&server)
            .await;

        let (cache, _dir, http, url) = build_cached_url(&server).await;
        cache.get_or_revalidate(&http, url.clone()).await.unwrap();
        let (body2, outcome2) = cache.get_or_revalidate(&http, url.clone()).await.unwrap();
        assert_eq!(body2, "hello");
        assert_eq!(outcome2, CacheOutcome::Hit);
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "second call must not touch the network"
        );
    }

    #[tokio::test]
    async fn after_expiry_revalidates_and_304_returns_cached_body() {
        let server = MockServer::start().await;
        let count = Arc::new(AtomicU64::new(0));
        let lm = http_date(Utc::now() - Duration::minutes(10));
        // First response is already expired.
        Mock::given(method("GET"))
            .and(path("/cached"))
            .respond_with(CountingResponder {
                body: "hello".into(),
                last_modified: lm.clone(),
                expires: Utc::now() - Duration::minutes(1),
                count: count.clone(),
            })
            .mount(&server)
            .await;

        let (cache, _dir, http, url) = build_cached_url(&server).await;
        let (b1, o1) = cache.get_or_revalidate(&http, url.clone()).await.unwrap();
        assert_eq!(o1, CacheOutcome::Refreshed);
        assert_eq!(b1, "hello");

        // Second call: cache is stale, send If-Modified-Since, get 304.
        let (b2, o2) = cache.get_or_revalidate(&http, url.clone()).await.unwrap();
        assert_eq!(o2, CacheOutcome::Revalidated);
        assert_eq!(b2, "hello");
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn after_expiry_with_changed_body_returns_new_body() {
        let server = MockServer::start().await;
        let count = Arc::new(AtomicU64::new(0));

        // Always serve a *different* Last-Modified so revalidation never
        // returns 304 — i.e. simulate "the upstream changed".
        struct AlwaysFresh {
            count: Arc<AtomicU64>,
        }
        impl Respond for AlwaysFresh {
            fn respond(&self, _: &Request) -> ResponseTemplate {
                let n = self.count.fetch_add(1, Ordering::SeqCst);
                let lm = Utc::now() - Duration::seconds(n as i64);
                ResponseTemplate::new(200)
                    .set_body_string(format!("body-{n}"))
                    .insert_header("last-modified", http_date(lm).as_str())
                    .insert_header(
                        "expires",
                        http_date(Utc::now() - Duration::minutes(1)).as_str(),
                    )
            }
        }

        Mock::given(method("GET"))
            .and(path("/cached"))
            .respond_with(AlwaysFresh {
                count: count.clone(),
            })
            .mount(&server)
            .await;

        let (cache, _dir, http, url) = build_cached_url(&server).await;
        let (b1, _) = cache.get_or_revalidate(&http, url.clone()).await.unwrap();
        let (b2, o2) = cache.get_or_revalidate(&http, url.clone()).await.unwrap();
        assert_eq!(b1, "body-0");
        assert_eq!(b2, "body-1");
        assert_eq!(o2, CacheOutcome::Refreshed);
    }

    /// Mount a one-off 200 with the given expiry so the cache holds a stale
    /// entry, then swap the server's response for the follow-up call.
    async fn prime_stale_entry(
        server: &MockServer,
        cache: &Cache,
        http: &reqwest::Client,
        url: &Url,
        expires: DateTime<Utc>,
    ) {
        let guard = Mock::given(method("GET"))
            .and(path("/cached"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("stale-body")
                    .insert_header(
                        "last-modified",
                        http_date(Utc::now() - Duration::minutes(10)).as_str(),
                    )
                    .insert_header("expires", http_date(expires).as_str()),
            )
            .mount_as_scoped(server)
            .await;
        let (body, outcome) = cache.get_or_revalidate(http, url.clone()).await.unwrap();
        assert_eq!(body, "stale-body");
        assert_eq!(outcome, CacheOutcome::Refreshed);
        drop(guard);
    }

    #[tokio::test]
    async fn server_error_within_grace_serves_stale_body() {
        let server = MockServer::start().await;
        let (cache, _dir, http, url) = build_cached_url(&server).await;
        prime_stale_entry(
            &server,
            &cache,
            &http,
            &url,
            Utc::now() - Duration::minutes(1),
        )
        .await;

        Mock::given(method("GET"))
            .and(path("/cached"))
            .respond_with(ResponseTemplate::new(503).set_body_string("down"))
            .mount(&server)
            .await;

        let (body, outcome) = cache.get_or_revalidate(&http, url).await.unwrap();
        assert_eq!(body, "stale-body");
        assert_eq!(outcome, CacheOutcome::StaleOnError);
    }

    #[tokio::test]
    async fn rate_limit_within_grace_serves_stale_body() {
        let server = MockServer::start().await;
        let (cache, _dir, http, url) = build_cached_url(&server).await;
        prime_stale_entry(
            &server,
            &cache,
            &http,
            &url,
            Utc::now() - Duration::minutes(1),
        )
        .await;

        Mock::given(method("GET"))
            .and(path("/cached"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;

        let (_, outcome) = cache.get_or_revalidate(&http, url).await.unwrap();
        assert_eq!(outcome, CacheOutcome::StaleOnError);
    }

    #[tokio::test]
    async fn server_error_past_grace_propagates() {
        let server = MockServer::start().await;
        let (cache, _dir, http, url) = build_cached_url(&server).await;
        prime_stale_entry(
            &server,
            &cache,
            &http,
            &url,
            Utc::now() - Duration::hours(STALE_GRACE_HOURS + 1),
        )
        .await;

        Mock::given(method("GET"))
            .and(path("/cached"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let err = cache.get_or_revalidate(&http, url).await.unwrap_err();
        match err {
            ClientError::Http { status: 503, .. } => {}
            other => panic!("expected 503 past grace, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn client_error_with_stale_entry_propagates() {
        let server = MockServer::start().await;
        let (cache, _dir, http, url) = build_cached_url(&server).await;
        prime_stale_entry(
            &server,
            &cache,
            &http,
            &url,
            Utc::now() - Duration::minutes(1),
        )
        .await;

        // 403 = banned User-Agent per the MET TOS — never paper over it.
        Mock::given(method("GET"))
            .and(path("/cached"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let err = cache.get_or_revalidate(&http, url).await.unwrap_err();
        match err {
            ClientError::Http { status: 403, .. } => {}
            other => panic!("expected 403, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn network_error_within_grace_serves_stale_body() {
        let dir = TempDir::new().unwrap();
        let cache = Cache::new(dir.path().to_path_buf());
        let http = reqwest::Client::builder().build().unwrap();

        // Point at a port nothing listens on, with a stale entry pre-written,
        // so the revalidation GET fails with a connection error.
        let url = Url::parse("http://127.0.0.1:9/cached").unwrap();
        cache
            .write_entry(
                &url,
                &CacheEntry {
                    last_modified: None,
                    expires: Some(Utc::now() - Duration::minutes(1)),
                    body: "stale-body".into(),
                },
            )
            .await
            .unwrap();

        let (body, outcome) = cache.get_or_revalidate(&http, url).await.unwrap();
        assert_eq!(body, "stale-body");
        assert_eq!(outcome, CacheOutcome::StaleOnError);
    }

    #[tokio::test]
    async fn http_error_is_propagated_without_writing_cache() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/broken"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let dir = TempDir::new().unwrap();
        let cache = Cache::new(dir.path().to_path_buf());
        let http = reqwest::Client::builder().build().unwrap();
        let url = Url::parse(&format!("{}/broken", server.uri())).unwrap();

        let err = cache
            .get_or_revalidate(&http, url.clone())
            .await
            .unwrap_err();
        match err {
            ClientError::Http { status: 500, .. } => {}
            other => panic!("expected 500, got {other:?}"),
        }
        assert!(!entry_exists(dir.path(), &url), "no cache entry on error");
    }
}
