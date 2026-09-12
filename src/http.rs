// Licensed under the Apache License, Version 2.0.

//! The one seam every vendor adapter goes through.
//!
//! Seven platforms, seven REST dialects, and no way to run any of them in CI. So
//! HTTP is a trait: adapters are written against it, `Reqwest` implements it for
//! real use, and `Recorded` replays fixtures in tests. That is what makes it
//! possible to assert that the Power BI adapter sends its token as a header and
//! asks for the right endpoint, without a Power BI tenant.

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::Value;
use std::time::Duration;

#[async_trait]
pub trait Http: Send + Sync {
    async fn get_json(&self, url: &str, headers: &[(String, String)]) -> Result<Value>;

    async fn post_json(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: Value,
    ) -> Result<Value>;

    /// For image endpoints, which return bytes and a content type.
    async fn get_bytes(&self, url: &str, headers: &[(String, String)])
    -> Result<(Vec<u8>, String)>;
}

pub struct Reqwest {
    client: reqwest::Client,
}

impl Default for Reqwest {
    fn default() -> Self {
        Self::new()
    }
}

impl Reqwest {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .user_agent("mcp-bi")
                // Superset's CSRF check wants the session cookie issued with the
                // token, so cookies have to survive between the two calls.
                .cookie_store(true)
                .build()
                .unwrap_or_default(),
        }
    }
}

fn apply(
    mut request: reqwest::RequestBuilder,
    headers: &[(String, String)],
) -> reqwest::RequestBuilder {
    for (name, value) in headers {
        request = request.header(name.as_str(), value.as_str());
    }
    request
}

/// The HTTP status of a failed call, attached so a caller can react to it.
///
/// The status is already in the error text, but reacting to a token expiry by matching
/// on a message means a reworded message silently disables the recovery. This carries it
/// as data. Metabase makes the case concrete: an expired session answers `401` with the
/// bare body `Unauthenticated`, which is not JSON and says nothing a parser can use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpStatus(pub u16);

impl std::fmt::Display for HttpStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HTTP status {}", self.0)
    }
}

impl std::error::Error for HttpStatus {}

/// The status of a failed call, when it failed with one.
#[must_use]
pub fn status_of(error: &anyhow::Error) -> Option<u16> {
    // anyhow's own downcast, not a walk over `chain()`. Iterating the chain yields the
    // internal context wrapper as `&dyn Error`, whose concrete type is that wrapper
    // rather than the value inside it, so `downcast_ref::<HttpStatus>()` on a chain
    // element never matches. `Error::downcast_ref` knows to look at attached context.
    error
        .downcast_ref::<HttpStatus>()
        .map(|HttpStatus(code)| *code)
}

/// True when a call failed because the credential was not accepted.
///
/// Both codes matter: a platform that has forgotten the session answers 401, and one
/// that still knows it but has downgraded its rights answers 403. Re-authenticating is
/// worth trying for either, since a fresh session is what would fix both.
#[must_use]
pub fn is_unauthorized(error: &anyhow::Error) -> bool {
    matches!(status_of(error), Some(401 | 403))
}

/// A failed call reports the status and the body, because a BI platform's error
/// body is usually the only thing that says which permission is missing.
async fn json_or_error(response: reqwest::Response, url: &str) -> Result<Value> {
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(
            anyhow::Error::new(HttpStatus(status.as_u16())).context(format!(
                "{url} returned HTTP {status}: {}",
                text.chars().take(400).collect::<String>()
            )),
        );
    }
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text).map_err(|error| {
        anyhow!(
            "{url} did not return JSON ({error}): {}",
            text.chars().take(200).collect::<String>()
        )
    })
}

#[async_trait]
impl Http for Reqwest {
    async fn get_json(&self, url: &str, headers: &[(String, String)]) -> Result<Value> {
        let response = apply(self.client.get(url), headers).send().await?;
        json_or_error(response, url).await
    }

    async fn post_json(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: Value,
    ) -> Result<Value> {
        let response = apply(self.client.post(url), headers)
            .json(&body)
            .send()
            .await?;
        json_or_error(response, url).await
    }

    async fn get_bytes(
        &self,
        url: &str,
        headers: &[(String, String)],
    ) -> Result<(Vec<u8>, String)> {
        let response = apply(self.client.get(url), headers).send().await?;
        let status = response.status();
        let mime = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("application/octet-stream")
            .split(';')
            .next()
            .unwrap_or("application/octet-stream")
            .to_string();
        let bytes = response.bytes().await?.to_vec();
        if !status.is_success() {
            return Err(anyhow::Error::new(HttpStatus(status.as_u16()))
                .context(format!("{url} returned HTTP {status}")));
        }
        Ok((bytes, mime))
    }
}

/// One observed request: where it went, what headers it carried, and its body.
#[cfg(any(test, feature = "recorded-http"))]
pub type Seen = (String, Vec<(String, String)>, Option<Value>);

/// A fixture-backed client, for tests.
///
/// Matches on a substring of the URL rather than the whole thing, so a test says
/// what it cares about — the endpoint — without restating query strings.
#[cfg(any(test, feature = "recorded-http"))]
pub struct Recorded {
    pub routes: Vec<(String, Value)>,
    pub images: Vec<(String, Vec<u8>, String)>,
    /// Every request seen, so a test can assert on headers actually sent.
    pub seen: std::sync::Mutex<Vec<Seen>>,
    /// Routes that answer 401 a set number of times before succeeding, so the
    /// recovery path can be exercised without a platform to expire a session on.
    lapsing: std::sync::Mutex<Vec<(String, usize)>>,
}

#[cfg(any(test, feature = "recorded-http"))]
impl Recorded {
    pub fn new(routes: Vec<(&str, Value)>) -> Self {
        Self {
            routes: routes
                .into_iter()
                .map(|(pattern, body)| (pattern.to_string(), body))
                .collect(),
            images: Vec::new(),
            seen: std::sync::Mutex::new(Vec::new()),
            lapsing: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn with_image(mut self, pattern: &str, bytes: Vec<u8>, mime: &str) -> Self {
        self.images
            .push((pattern.to_string(), bytes, mime.to_string()));
        self
    }

    /// Make a route answer 401 the first `times` requests, then succeed.
    #[must_use]
    pub fn lapsing_for(self, pattern: &str, times: usize) -> Self {
        if let Ok(mut lapsing) = self.lapsing.lock() {
            lapsing.push((pattern.to_string(), times));
        }
        self
    }

    fn find(&self, url: &str) -> Result<Value> {
        // A route registered as lapsing answers 401 the first time it is asked, exactly
        // as a platform does when the session it was given has expired. That is the only
        // way to test recovery, because a session id carries no expiry to fast-forward.
        if let Ok(mut lapsing) = self.lapsing.lock()
            && let Some(remaining) = lapsing
                .iter_mut()
                .find(|(pattern, remaining)| url.contains(pattern.as_str()) && *remaining > 0)
                .map(|(_, remaining)| remaining)
        {
            *remaining -= 1;
            return Err(anyhow::Error::new(HttpStatus(401)).context(format!(
                "{url} returned HTTP 401 Unauthorized: Unauthenticated"
            )));
        }
        self.routes
            .iter()
            .find(|(pattern, _)| url.contains(pattern.as_str()))
            .map(|(_, body)| body.clone())
            .ok_or_else(|| anyhow!("no recorded response for {url}"))
    }

    fn record(&self, url: &str, headers: &[(String, String)], body: Option<Value>) {
        if let Ok(mut seen) = self.seen.lock() {
            seen.push((url.to_string(), headers.to_vec(), body));
        }
    }

    /// Did any request carry this header?
    pub fn sent_header(&self, name: &str, value: &str) -> bool {
        self.seen
            .lock()
            .map(|seen| {
                seen.iter().any(|(_, headers, _)| {
                    headers.iter().any(|(header, actual)| {
                        header.eq_ignore_ascii_case(name) && actual == value
                    })
                })
            })
            .unwrap_or(false)
    }

    /// Was a URL containing this fragment requested?
    pub fn called(&self, fragment: &str) -> bool {
        self.seen
            .lock()
            .map(|seen| seen.iter().any(|(url, _, _)| url.contains(fragment)))
            .unwrap_or(false)
    }
}

#[cfg(any(test, feature = "recorded-http"))]
#[async_trait]
impl Http for Recorded {
    async fn get_json(&self, url: &str, headers: &[(String, String)]) -> Result<Value> {
        self.record(url, headers, None);
        self.find(url)
    }

    async fn post_json(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: Value,
    ) -> Result<Value> {
        self.record(url, headers, Some(body));
        self.find(url)
    }

    async fn get_bytes(
        &self,
        url: &str,
        headers: &[(String, String)],
    ) -> Result<(Vec<u8>, String)> {
        self.record(url, headers, None);
        self.images
            .iter()
            .find(|(pattern, _, _)| url.contains(pattern.as_str()))
            .map(|(_, bytes, mime)| (bytes.clone(), mime.clone()))
            .ok_or_else(|| anyhow!("no recorded image for {url}"))
    }
}
