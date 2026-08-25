use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use anyhow::{Context, Result};

const BIND_ADDR_ENV: &str = "TWIN_OPENAI_BIND_ADDR";
const REQUIRE_AUTH_ENV: &str = "TWIN_OPENAI_REQUIRE_AUTH";
const ENABLE_ADMIN_ENV: &str = "TWIN_OPENAI_ENABLE_ADMIN";
const REQUEST_LOG_PATH_ENV: &str = "TWIN_OPENAI_REQUEST_LOG_PATH";

#[derive(Clone, Debug)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub require_auth: bool,
    pub enable_admin: bool,
    pub request_log_path: Option<PathBuf>,
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

        Ok(Self {
            bind_addr,
            require_auth,
            enable_admin,
            request_log_path,
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
