use anyhow::{Context, Result};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

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

pub fn parse_bool_env(value: &str, name: &str) -> Result<bool> {
    match value {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        _ => anyhow::bail!("{name} must be true/false or 1/0"),
    }
}

/// An empty override falls back to the provider's standard API key variable.
pub fn lookup_api_key(
    lookup: &dyn Fn(&str) -> Option<String>,
    override_env: &str,
    fallback_env: &str,
) -> Option<String> {
    lookup(override_env)
        .filter(|value| !value.is_empty())
        .or_else(|| lookup(fallback_env).filter(|value| !value.is_empty()))
}

pub fn validate_proxy_record(
    mode: Mode,
    key: Option<&str>,
    recording_path: Option<&Path>,
    override_env: &str,
    fallback_env: &str,
    recording_env: &str,
) -> Result<()> {
    if mode == Mode::ProxyRecord {
        anyhow::ensure!(
            key.is_some_and(|key| !key.trim().is_empty()),
            "proxy-record mode requires {override_env} or {fallback_env}"
        );
        anyhow::ensure!(
            recording_path.is_some(),
            "proxy-record mode requires {recording_env}"
        );
    }
    Ok(())
}

/// Provider-specific environment names and server defaults.
#[derive(Clone, Copy, Debug)]
pub struct ProviderConfig {
    pub env_prefix: &'static str,
    pub api_key_env: &'static str,
    pub upstream_path_env: &'static str,
    pub default_port: u16,
    pub default_upstream_url: &'static str,
}

/// Shared settings, mapped into each twin's public `Config` so callers can
/// keep using provider-specific fields such as `upstream_responses_path`.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "Each bool is an independent env-configured switch, not a state machine."
)]
pub struct Settings {
    pub bind_addr: SocketAddr,
    pub require_auth: bool,
    pub enable_admin: bool,
    pub request_log_path: Option<PathBuf>,
    pub scenarios_path: Option<PathBuf>,
    pub allow_unmatched: bool,
    pub mode: Mode,
    pub upstream_url: String,
    pub upstream_path: Option<String>,
    pub upstream_api_key: Option<String>,
    pub recording_path: Option<PathBuf>,
    pub record_format: RecordFormat,
    pub recording_append: bool,
}

impl ProviderConfig {
    pub fn defaults(&self) -> Settings {
        Settings {
            bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), self.default_port),
            require_auth: true,
            enable_admin: true,
            request_log_path: None,
            scenarios_path: None,
            allow_unmatched: false,
            mode: Mode::Twin,
            upstream_url: self.default_upstream_url.to_owned(),
            upstream_path: None,
            upstream_api_key: None,
            recording_path: None,
            record_format: RecordFormat::Semantic,
            recording_append: false,
        }
    }

    pub fn load(&self, lookup: &dyn Fn(&str) -> Option<String>) -> Result<Settings> {
        let mut settings = self.defaults();
        let get = |suffix: &str| lookup(&self.env_name(suffix));
        let nonempty = |suffix: &str| get(suffix).filter(|value| !value.is_empty());
        let boolean = |suffix: &str, default| {
            get(suffix)
                .map(|value| parse_bool_env(&value, &self.env_name(suffix)))
                .transpose()
                .map(|value| value.unwrap_or(default))
        };

        if let Some(value) = get("BIND_ADDR") {
            settings.bind_addr = value
                .parse()
                .with_context(|| format!("invalid {}", self.env_name("BIND_ADDR")))?;
        }
        settings.require_auth = boolean("REQUIRE_AUTH", settings.require_auth)?;
        settings.enable_admin = boolean("ENABLE_ADMIN", settings.enable_admin)?;
        settings.request_log_path = nonempty("REQUEST_LOG_PATH").map(PathBuf::from);
        settings.scenarios_path = nonempty("SCENARIOS_PATH").map(PathBuf::from);
        settings.allow_unmatched = boolean("ALLOW_UNMATCHED", settings.allow_unmatched)?;
        settings.mode = match get("MODE").as_deref() {
            None => settings.mode,
            Some("twin") => Mode::Twin,
            Some("proxy-record") => Mode::ProxyRecord,
            Some(other) => anyhow::bail!(
                "{} must be twin or proxy-record, got {other}",
                self.env_name("MODE")
            ),
        };
        if let Some(value) = nonempty("UPSTREAM_URL") {
            value
                .trim_end_matches('/')
                .clone_into(&mut settings.upstream_url);
        }
        settings.upstream_path = lookup(self.upstream_path_env).filter(|value| !value.is_empty());
        settings.upstream_api_key =
            lookup_api_key(lookup, &self.env_name("UPSTREAM_API_KEY"), self.api_key_env);
        settings.recording_path = nonempty("RECORDING_PATH").map(PathBuf::from);
        settings.record_format = match get("RECORD_FORMAT").as_deref() {
            None => settings.record_format,
            Some("semantic") => RecordFormat::Semantic,
            Some("transcript") => RecordFormat::Transcript,
            Some(other) => anyhow::bail!(
                "{} must be semantic or transcript, got {other}",
                self.env_name("RECORD_FORMAT")
            ),
        };
        settings.recording_append = boolean("RECORDING_APPEND", settings.recording_append)?;
        self.validate(
            settings.mode,
            settings.upstream_api_key.as_deref(),
            settings.recording_path.as_deref(),
            settings.upstream_path.as_deref(),
        )?;
        Ok(settings)
    }

    pub fn validate(
        &self,
        mode: Mode,
        upstream_api_key: Option<&str>,
        recording_path: Option<&Path>,
        upstream_path: Option<&str>,
    ) -> Result<()> {
        validate_proxy_record(
            mode,
            upstream_api_key,
            recording_path,
            &self.env_name("UPSTREAM_API_KEY"),
            self.api_key_env,
            &self.env_name("RECORDING_PATH"),
        )?;
        if let Some(path) = upstream_path {
            anyhow::ensure!(
                path.starts_with('/'),
                "{} must start with '/', got {path}",
                self.upstream_path_env
            );
        }
        Ok(())
    }

    fn env_name(&self, suffix: &str) -> String {
        format!("{}_{suffix}", self.env_prefix)
    }
}
