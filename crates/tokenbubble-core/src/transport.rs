//! Bounded, cookie-less, non-redirecting HTTP GET used by every collector.

use std::fmt;
use std::time::Duration;

use crate::secret::Secret;

/// An authenticated GET request. `Debug` never shows the bearer token.
#[derive(Clone, Debug)]
pub struct Request {
    pub url: String,
    pub bearer: Option<Secret>,
    pub headers: Vec<(&'static str, String)>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
    /// `Retry-After` in delta-seconds form; HTTP-date values are ignored.
    pub retry_after: Option<Duration>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransportError {
    Timeout,
    BodyTooLarge {
        limit: u64,
    },
    /// A 3xx response. Redirects are never followed so credentials cannot leave the origin.
    Redirect {
        status: u16,
    },
    Network(String),
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout => f.write_str("request timed out"),
            Self::BodyTooLarge { limit } => write!(f, "response exceeded {limit} bytes"),
            Self::Redirect { status } => write!(f, "refused to follow HTTP {status} redirect"),
            Self::Network(message) => write!(f, "network error: {message}"),
        }
    }
}

impl std::error::Error for TransportError {}

pub trait Transport: Send + Sync {
    fn get(&self, request: &Request) -> Result<Response, TransportError>;
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Whole-request deadline including connect, TLS, and body.
    pub timeout: Duration,
    pub max_body_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(20),
            max_body_bytes: 1024 * 1024,
        }
    }
}

/// Production transport: rustls, environment proxies, no cookies, no redirects.
pub struct HttpTransport {
    agent: ureq::Agent,
    limits: Limits,
}

impl HttpTransport {
    pub fn new(limits: Limits) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(limits.timeout))
            .max_redirects(0)
            .http_status_as_error(false)
            .user_agent(concat!("TokenBubble/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Self { agent, limits }
    }
}

impl Default for HttpTransport {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl Transport for HttpTransport {
    fn get(&self, request: &Request) -> Result<Response, TransportError> {
        let mut builder = self.agent.get(&request.url);
        if let Some(bearer) = &request.bearer {
            builder = builder.header("Authorization", format!("Bearer {}", bearer.expose()));
        }
        for (name, value) in &request.headers {
            builder = builder.header(*name, value);
        }

        let mut response = builder
            .call()
            .map_err(|error| map_error(error, self.limits))?;
        let status = response.status().as_u16();
        if (300..400).contains(&status) {
            return Err(TransportError::Redirect { status });
        }
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let body = response
            .body_mut()
            .with_config()
            .limit(self.limits.max_body_bytes)
            .read_to_vec()
            .map_err(|error| map_error(error, self.limits))?;
        Ok(Response {
            status,
            body,
            retry_after,
        })
    }
}

fn map_error(error: ureq::Error, limits: Limits) -> TransportError {
    match error {
        ureq::Error::Timeout(_) => TransportError::Timeout,
        ureq::Error::BodyExceedsLimit(_) => TransportError::BodyTooLarge {
            limit: limits.max_body_bytes,
        },
        ureq::Error::Io(io) if io.kind() == std::io::ErrorKind::TimedOut => TransportError::Timeout,
        other => TransportError::Network(other.to_string()),
    }
}
