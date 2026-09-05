use anyhow::Result;
use std::path::Path;

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
