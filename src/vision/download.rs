//! HTTPS acquisition for pinned local model artifacts.

use std::fmt;
use std::io::Read;
use std::time::Duration;

use thiserror::Error;

use super::{ModelArtifact, ModelDownloader};

/// Default end-to-end timeout for one model artifact request.
pub const DEFAULT_MODEL_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Streaming HTTPS downloader used by lazy model resolution.
#[derive(Clone)]
pub struct HttpModelDownloader {
    agent: ureq::Agent,
}

impl fmt::Debug for HttpModelDownloader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpModelDownloader")
            .finish_non_exhaustive()
    }
}

impl Default for HttpModelDownloader {
    fn default() -> Self {
        Self::new(DEFAULT_MODEL_DOWNLOAD_TIMEOUT)
    }
}

impl HttpModelDownloader {
    /// Creates an HTTPS-only downloader with an end-to-end request timeout.
    pub fn new(timeout: Duration) -> Self {
        let config = ureq::Agent::config_builder()
            .https_only(true)
            .timeout_global(Some(timeout))
            .user_agent(concat!("pdf-inspector/", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
        }
    }
}

impl ModelDownloader for HttpModelDownloader {
    type Error = HttpModelDownloadError;

    fn open(&self, artifact: &ModelArtifact) -> Result<Box<dyn Read + Send>, Self::Error> {
        let response = self.agent.get(artifact.url).call()?;
        if let Some(actual) = response.body().content_length() {
            if actual != artifact.size {
                return Err(HttpModelDownloadError::ContentLength {
                    expected: artifact.size,
                    actual,
                });
            }
        }

        // One extra byte lets ModelStore report an exact size mismatch while
        // preventing a malicious or broken server from filling the disk.
        let limit = artifact.size.saturating_add(1);
        Ok(Box::new(response.into_body().into_reader().take(limit)))
    }
}

/// Failures before a response stream reaches [`super::ModelStore`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum HttpModelDownloadError {
    /// DNS, TLS, redirect, HTTP status, or response-stream setup failed.
    #[error(transparent)]
    Request(#[from] ureq::Error),
    /// The server declared a size that disagrees with the pinned manifest.
    #[error("server declared {actual} bytes; manifest requires {expected}")]
    ContentLength {
        /// Pinned artifact size.
        expected: u64,
        /// Server-declared size.
        actual: u64,
    },
}
