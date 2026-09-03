use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use anyhow::{Context, Result};

const BIND_ADDR_ENV: &str = "TWIN_OPENAI_BIND_ADDR";
const REQUIRE_AUTH_ENV: &str = "TWIN_OPENAI_REQUIRE_AUTH";
const ENABLE_ADMIN_ENV: &str = "TWIN_OPENAI_ENABLE_ADMIN";
const REQUEST_LOG_PATH_ENV: &str = "TWIN_OPENAI_REQUEST_LOG_PATH";
const SCENARIOS_PATH_ENV: &str = "TWIN_OPENAI_SCENARIOS_PATH";
const ALLOW_UNMATCHED_ENV: &str = "TWIN_OPENAI_ALLOW_UNMATCHED";
const MODE_ENV: &str = "TWIN_OPENAI_MODE";
const UPSTREAM_URL_ENV: &str = "TWIN_OPENAI_UPSTREAM_URL";
const UPSTREAM_RESPONSES_PATH_ENV: &str = "TWIN_OPENAI_UPSTREAM_RESPONSES_PATH";
const UPSTREAM_API_KEY_ENV: &str = "TWIN_OPENAI_UPSTREAM_API_KEY";
const OPENAI_API_KEY_ENV: &str = "OPENAI_API_KEY";
const RECORDING_PATH_ENV: &str = "TWIN_OPENAI_RECORDING_PATH";
const RECORD_FORMAT_ENV: &str = "TWIN_OPENAI_RECORD_FORMAT";
const RECORDING_APPEND_ENV: &str = "TWIN_OPENAI_RECORDING_APPEND";

const DEFAULT_UPSTREAM_URL: &str = "https://api.openai.com";

/// How the server treats `/v1/*` traffic.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Mode {
    /// Serve deterministic twin responses (scenarios and fallbacks).
    #[default]
    Twin,
    /// Forward requests to a real upstream, stream responses back verbatim,
    /// and derive a scenario recording from every successful exchange.
    ProxyRecord,
}

/// What proxy-record mode writes for each successful exchange.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RecordFormat {
    /// Derive the exchange into the canonical scenario shape: response
    /// text, tool calls, usage. Replays through the twin's engine.
    #[default]
    Semantic,
    /// Keep the exchange verbatim: status, content type, and the raw JSON
    /// body or the ordered SSE events. Replays byte-faithfully, preserving
    /// provider extension fields and event granularity the canonical
    /// engine would erase.
    Transcript,
}

#[derive(Clone, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "Each bool is an independent env-configured switch, not a state machine."
)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub require_auth: bool,
    pub enable_admin: bool,
    pub request_log_path: Option<PathBuf>,
    pub scenarios_path: Option<PathBuf>,
    pub allow_unmatched: bool,
    pub mode: Mode,
    pub upstream_url: String,
    /// The upstream path `/v1/responses` traffic is forwarded to, when the
    /// upstream hangs its Responses endpoint somewhere else. OpenAI's Codex
    /// deployment serves the unversioned `<base>/responses`. `None` keeps
    /// the default `/v1/responses`.
    pub upstream_responses_path: Option<String>,
    pub upstream_api_key: Option<String>,
    pub recording_path: Option<PathBuf>,
    pub record_format: RecordFormat,
    pub recording_append: bool,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(&process_env_var)
    }

    pub fn from_lookup(lookup: &dyn Fn(&str) -> Option<String>) -> Result<Self> {
        let bind_addr = lookup(BIND_ADDR_ENV)
            .map(|value| value.parse().context("invalid TWIN_OPENAI_BIND_ADDR"))
            .transpose()?
            .unwrap_or_else(|| SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 3000));

        let require_auth = lookup(REQUIRE_AUTH_ENV)
            .map(|value| parse_bool_env(&value, REQUIRE_AUTH_ENV))
            .transpose()?
            .unwrap_or(true);

        let enable_admin = lookup(ENABLE_ADMIN_ENV)
            .map(|value| parse_bool_env(&value, ENABLE_ADMIN_ENV))
            .transpose()?
            .unwrap_or(true);

        let request_log_path = lookup(REQUEST_LOG_PATH_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let scenarios_path = lookup(SCENARIOS_PATH_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let allow_unmatched = lookup(ALLOW_UNMATCHED_ENV)
            .map(|value| parse_bool_env(&value, ALLOW_UNMATCHED_ENV))
            .transpose()?
            .unwrap_or(false);

        let mode = match lookup(MODE_ENV).as_deref() {
            None | Some("twin") => Mode::Twin,
            Some("proxy-record") => Mode::ProxyRecord,
            Some(other) => {
                anyhow::bail!("{MODE_ENV} must be twin or proxy-record, got {other}")
            }
        };
        let upstream_url = lookup(UPSTREAM_URL_ENV)
            .filter(|value| !value.is_empty())
            .map_or_else(
                || DEFAULT_UPSTREAM_URL.to_owned(),
                |value| value.trim_end_matches('/').to_owned(),
            );
        let upstream_responses_path =
            lookup(UPSTREAM_RESPONSES_PATH_ENV).filter(|value| !value.is_empty());
        let upstream_api_key = lookup(UPSTREAM_API_KEY_ENV)
            .or_else(|| lookup(OPENAI_API_KEY_ENV))
            .filter(|value| !value.is_empty());
        let recording_path = lookup(RECORDING_PATH_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let record_format = match lookup(RECORD_FORMAT_ENV).as_deref() {
            None | Some("semantic") => RecordFormat::Semantic,
            Some("transcript") => RecordFormat::Transcript,
            Some(other) => {
                anyhow::bail!("{RECORD_FORMAT_ENV} must be semantic or transcript, got {other}")
            }
        };
        let recording_append = lookup(RECORDING_APPEND_ENV)
            .map(|value| parse_bool_env(&value, RECORDING_APPEND_ENV))
            .transpose()?
            .unwrap_or(false);

        let config = Self {
            bind_addr,
            require_auth,
            enable_admin,
            request_log_path,
            scenarios_path,
            allow_unmatched,
            mode,
            upstream_url,
            upstream_responses_path,
            upstream_api_key,
            recording_path,
            record_format,
            recording_append,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.mode == Mode::ProxyRecord {
            anyhow::ensure!(
                self.upstream_api_key.is_some(),
                "proxy-record mode requires {UPSTREAM_API_KEY_ENV} or {OPENAI_API_KEY_ENV}"
            );
            anyhow::ensure!(
                self.recording_path.is_some(),
                "proxy-record mode requires {RECORDING_PATH_ENV}"
            );
        }
        if let Some(path) = &self.upstream_responses_path {
            anyhow::ensure!(
                path.starts_with('/'),
                "{UPSTREAM_RESPONSES_PATH_ENV} must start with '/', got {path}"
            );
        }
        Ok(())
    }
}

fn process_env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

impl Default for Config {
    fn default() -> Self {
        Self::from_env().unwrap_or(Self {
            bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 3000),
            require_auth: true,
            enable_admin: true,
            request_log_path: None,
            scenarios_path: None,
            allow_unmatched: false,
            mode: Mode::Twin,
            upstream_url: DEFAULT_UPSTREAM_URL.to_owned(),
            upstream_responses_path: None,
            upstream_api_key: None,
            recording_path: None,
            record_format: RecordFormat::Semantic,
            recording_append: false,
        })
    }
}

fn parse_bool_env(value: &str, name: &str) -> Result<bool> {
    match value {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        _ => anyhow::bail!("{name} must be true/false or 1/0"),
    }
}
