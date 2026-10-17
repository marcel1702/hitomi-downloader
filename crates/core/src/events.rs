use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use specta::Type;

use crate::{
    download_manager::DownloadTaskState,
    types::{Comic, LogLevel},
};

/// Receives the events the core sends to the frontend
///
/// The desktop app forwards them to the webview through Tauri, a server to its clients.
pub trait EventSink: Send + Sync {
    fn emit(&self, event: Event);
}

/// Every event the core sends to the frontend
#[derive(Debug, Clone)]
pub enum Event {
    Log(LogEvent),
    DownloadTask(DownloadTaskEvent),
    DownloadSpeed(DownloadSpeedEvent),
    ExportPdf(ExportPdfEvent),
    ExportCbz(ExportCbzEvent),
}

impl Event {
    /// The name the frontend listens to, the same name tauri-specta derives from the type
    pub fn name(&self) -> &'static str {
        match self {
            Event::Log(_) => "log-event",
            Event::DownloadTask(_) => "download-task-event",
            Event::DownloadSpeed(_) => "download-speed-event",
            Event::ExportPdf(_) => "export-pdf-event",
            Event::ExportCbz(_) => "export-cbz-event",
        }
    }

    /// The payload of the event, as the frontend receives it
    pub fn payload(&self) -> serde_json::Result<serde_json::Value> {
        match self {
            Event::Log(event) => serde_json::to_value(event),
            Event::DownloadTask(event) => serde_json::to_value(event),
            Event::DownloadSpeed(event) => serde_json::to_value(event),
            Event::ExportPdf(event) => serde_json::to_value(event),
            Event::ExportCbz(event) => serde_json::to_value(event),
        }
    }
}

macro_rules! impl_from_for_event {
    ($($variant:ident($ty:ty)),* $(,)?) => {
        $(impl From<$ty> for Event {
            fn from(event: $ty) -> Self {
                Event::$variant(event)
            }
        })*
    };
}

impl_from_for_event!(
    Log(LogEvent),
    DownloadTask(DownloadTaskEvent),
    DownloadSpeed(DownloadSpeedEvent),
    ExportPdf(ExportPdfEvent),
    ExportCbz(ExportCbzEvent),
);

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[cfg_attr(feature = "tauri", derive(tauri_specta::Event))]
#[serde(rename_all = "camelCase")]
pub struct LogEvent {
    pub timestamp: String,
    pub level: LogLevel,
    pub fields: HashMap<String, serde_json::Value>,
    pub target: String,
    pub filename: String,
    #[serde(rename = "line_number")]
    pub line_number: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[cfg_attr(feature = "tauri", derive(tauri_specta::Event))]
#[serde(tag = "event", content = "data")]
pub enum DownloadTaskEvent {
    #[serde(rename_all = "camelCase")]
    Create {
        state: DownloadTaskState,
        comic: Box<Comic>,
        downloaded_img_count: u32,
        total_img_count: u32,
    },

    #[serde(rename_all = "camelCase")]
    Update {
        comic_id: i32,
        state: DownloadTaskState,
        downloaded_img_count: u32,
        total_img_count: u32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[cfg_attr(feature = "tauri", derive(tauri_specta::Event))]
#[serde(rename_all = "camelCase")]
pub struct DownloadSpeedEvent {
    pub speed: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[cfg_attr(feature = "tauri", derive(tauri_specta::Event))]
#[serde(tag = "event", content = "data")]
pub enum ExportPdfEvent {
    #[serde(rename_all = "camelCase")]
    Start { uuid: String, title: String },
    #[serde(rename_all = "camelCase")]
    Error { uuid: String },
    #[serde(rename_all = "camelCase")]
    End { uuid: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[cfg_attr(feature = "tauri", derive(tauri_specta::Event))]
#[serde(tag = "event", content = "data")]
pub enum ExportCbzEvent {
    #[serde(rename_all = "camelCase")]
    Start { uuid: String, title: String },
    #[serde(rename_all = "camelCase")]
    Error { uuid: String },
    #[serde(rename_all = "camelCase")]
    End { uuid: String },
}
