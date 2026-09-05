use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Result;
pub use twin_core::config::{Mode, RecordFormat};
use twin_core::config::{ProviderConfig, Settings};

const PROVIDER: ProviderConfig = ProviderConfig {
    env_prefix: "TWIN_OPENAI",
    api_key_env: "OPENAI_API_KEY",
    upstream_path_env: "TWIN_OPENAI_UPSTREAM_RESPONSES_PATH",
    default_port: 3000,
    default_upstream_url: "https://api.openai.com",
};

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
        Self::from_lookup(&|name| std::env::var(name).ok())
    }

    pub fn from_lookup(lookup: &dyn Fn(&str) -> Option<String>) -> Result<Self> {
        PROVIDER.load(lookup).map(Self::from)
    }

    pub fn validate(&self) -> Result<()> {
        PROVIDER.validate(
            self.mode,
            self.upstream_api_key.as_deref(),
            self.recording_path.as_deref(),
            self.upstream_responses_path.as_deref(),
        )
    }
}

impl From<Settings> for Config {
    fn from(settings: Settings) -> Self {
        Self {
            bind_addr: settings.bind_addr,
            require_auth: settings.require_auth,
            enable_admin: settings.enable_admin,
            request_log_path: settings.request_log_path,
            scenarios_path: settings.scenarios_path,
            allow_unmatched: settings.allow_unmatched,
            mode: settings.mode,
            upstream_url: settings.upstream_url,
            upstream_responses_path: settings.upstream_path,
            upstream_api_key: settings.upstream_api_key,
            recording_path: settings.recording_path,
            record_format: settings.record_format,
            recording_append: settings.recording_append,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::from_env().unwrap_or_else(|_| PROVIDER.defaults().into())
    }
}
