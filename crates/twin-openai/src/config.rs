use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use anyhow::{Context, Result};

const BIND_ADDR_ENV: &str = "TWIN_OPENAI_BIND_ADDR";
const REQUIRE_AUTH_ENV: &str = "TWIN_OPENAI_REQUIRE_AUTH";
const ENABLE_ADMIN_ENV: &str = "TWIN_OPENAI_ENABLE_ADMIN";
const REQUEST_LOG_PATH_ENV: &str = "TWIN_OPENAI_REQUEST_LOG_PATH";
const SCENARIOS_PATH_ENV: &str = "TWIN_OPENAI_SCENARIOS_PATH";
const ALLOW_UNMATCHED_ENV: &str = "TWIN_OPENAI_ALLOW_UNMATCHED";

#[derive(Clone, Debug)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub require_auth: bool,
    pub enable_admin: bool,
    pub request_log_path: Option<PathBuf>,
    pub scenarios_path: Option<PathBuf>,
    pub allow_unmatched: bool,
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

        Ok(Self {
            bind_addr,
            require_auth,
            enable_admin,
            request_log_path,
            scenarios_path,
            allow_unmatched,
        })
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
