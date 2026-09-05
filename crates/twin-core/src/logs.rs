use anyhow::{Context, Result};
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;

use serde::Serialize;
use serde_json::{Map, Value};

#[derive(Clone, Debug, Serialize)]
pub struct RequestLog {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scenario_id: Option<String>,
    pub endpoint: String,
    pub model: String,
    pub stream: bool,
    pub input_text: String,
    pub instructions_text: String,
    pub metadata: Map<String, Value>,
}

#[derive(Debug)]
pub(crate) struct JsonlRequestLogWriter {
    writer: BufWriter<File>,
    twin_name: &'static str,
}

impl JsonlRequestLogWriter {
    pub(crate) fn open(path: &Path, twin_name: &'static str) -> Result<Self> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create {twin_name} request log directory {}",
                    parent.display()
                )
            })?;
        }

        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)
            .with_context(|| {
                format!(
                    "failed to create {twin_name} request log {}",
                    path.display()
                )
            })?;
        Ok(Self {
            writer: BufWriter::new(file),
            twin_name,
        })
    }

    pub(crate) fn write_record(&mut self, request: &RequestLog) -> Result<()> {
        let twin_name = self.twin_name;
        serde_json::to_writer(&mut self.writer, request)
            .with_context(|| format!("failed to serialize {twin_name} request log record"))?;
        self.writer
            .write_all(b"\n")
            .with_context(|| format!("failed to terminate {twin_name} request log record"))?;
        self.writer
            .flush()
            .with_context(|| format!("failed to flush {twin_name} request log record"))
    }
}
