use crate::anthropic::models::AnthropicError;
use crate::state::NamespaceKey;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

pub fn api_key(headers: &HeaderMap, required: bool) -> Result<Option<String>, Response> {
    let error = || {
        AnthropicError::new(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "missing, empty, or conflicting API credentials",
        )
        .into_response()
    };
    let key = headers
        .get("x-api-key")
        .map(|v| {
            v.to_str()
                .ok()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(error)
        })
        .transpose()?;
    let bearer = headers
        .get("authorization")
        .map(|v| {
            v.to_str()
                .ok()
                .and_then(|s| s.strip_prefix("Bearer "))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(error)
        })
        .transpose()?;
    if key.zip(bearer).is_some_and(|(a, b)| a != b) {
        return Err(error());
    }
    let token = key.or(bearer);
    if required && token.is_none() {
        return Err(error());
    }
    Ok(token.map(ToOwned::to_owned))
}

pub fn request_namespace(headers: &HeaderMap, required: bool) -> Result<NamespaceKey, Response> {
    Ok(api_key(headers, required)?.map_or(NamespaceKey::Global, NamespaceKey::ApiKey))
}

pub fn admin_request_namespace(headers: &HeaderMap) -> Result<NamespaceKey, Response> {
    request_namespace(headers, false)
}

pub fn validate_version(headers: &HeaderMap) -> Result<(), AnthropicError> {
    match headers
        .get("anthropic-version")
        .and_then(|v| v.to_str().ok())
    {
        Some("2023-06-01") => Ok(()),
        _ => Err(AnthropicError::invalid_request(
            "anthropic-version",
            "must be 2023-06-01",
        )),
    }
}
