#![allow(
    dead_code,
    reason = "Shared helpers are used by different integration tests."
)]
use axum::Router;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::{net::TcpListener, task::JoinHandle};
use twin_anthropic::config::Config;

pub struct Server {
    pub url: String,
    pub client: reqwest::Client,
    task: JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub fn config() -> Config {
    Config::from_lookup(&|_| None).expect("default config")
}
pub async fn spawn(config: Config) -> Server {
    spawn_router(twin_anthropic::build_app_with_config(config).expect("app")).await
}
pub async fn spawn_router(router: Router) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("address"));
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve");
    });
    Server {
        url,
        client: reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client"),
        task,
    }
}
impl Server {
    pub async fn post(&self, path: &str, key: &str, body: &Value) -> reqwest::Response {
        self.client
            .post(format!("{}{path}", self.url))
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01")
            .json(body)
            .send()
            .await
            .expect("post")
    }
    pub async fn get(&self, path: &str, key: &str) -> reqwest::Response {
        self.client
            .get(format!("{}{path}", self.url))
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01")
            .send()
            .await
            .expect("get")
    }
    pub async fn enqueue(&self, key: &str, scenarios: Value) {
        let r = self
            .post("/__admin/scenarios", key, &json!({"scenarios":scenarios}))
            .await;
        assert_eq!(r.status(), 200, "{}", r.text().await.expect("error body"));
    }
    pub async fn message(&self, key: &str, request: &Value) -> Value {
        let r = self.post("/v1/messages", key, request).await;
        let status = r.status();
        let body: Value = r.json().await.expect("JSON");
        assert_eq!(status, 200, "{body}");
        body
    }
    pub async fn logs(&self, key: &str) -> Value {
        self.get("/__admin/requests", key)
            .await
            .json()
            .await
            .expect("logs")
    }
}
pub fn request(stream: bool) -> Value {
    json!({"model":"claude-test","max_tokens":128,"messages":[{"role":"user","content":"hello"}],"stream":stream})
}
pub fn scenario(script: Value) -> Value {
    let mut value = json!({"matcher":{"endpoint":"messages"}});
    value["script"] = script;
    value
}
pub fn text(body: &Value) -> &str {
    body["content"]
        .as_array()
        .expect("content")
        .iter()
        .find_map(|v| v["text"].as_str())
        .unwrap_or_default()
}
static NEXT: AtomicU64 = AtomicU64::new(0);
pub struct TempFile(pub PathBuf);
impl TempFile {
    pub fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "twin-anthropic-test-{}-{}.json",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )))
    }
    pub fn write(&self, value: &Value) {
        std::fs::write(
            &self.0,
            serde_json::to_vec_pretty(value).expect("serialize"),
        )
        .expect("write");
    }
}
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub mod contracts;
