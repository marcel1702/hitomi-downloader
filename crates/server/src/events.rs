//! `GET /api/events`: the events of the core as Server-Sent Events, to every client at once
use std::{convert::Infallible, sync::Arc};

use axum::{
    extract::State,
    response::sse::{Event as SseEvent, KeepAlive, Sse},
};
use futures::{stream, Stream, StreamExt};
use hitomi_core::{
    download_manager::DownloadTaskState,
    events::{DownloadTaskEvent, Event, EventSink},
};
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;

use crate::state::{AppState, ComicCache};

/// How many events a slow client may fall behind before it misses some
///
/// The logs alone send an event per downloaded image, a client that lags this far behind
/// misses a few and catches up with the next progress update.
pub const CHANNEL_CAPACITY: usize = 4096;

/// Hands every event of the core to all connected clients
pub struct BroadcastSink {
    pub events: broadcast::Sender<Event>,
    pub comics: Arc<ComicCache>,
}

impl EventSink for BroadcastSink {
    fn emit(&self, event: Event) {
        if let Event::DownloadTask(DownloadTaskEvent::Update {
            comic_id,
            state: DownloadTaskState::Completed,
            ..
        }) = &event
        {
            // the comic is on disk now, its images are served from there from here on
            self.comics.remove(*comic_id);
        }
        // fails only while no client is connected, then there is no one to tell
        let _ = self.events.send(event);
    }
}

pub async fn events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    // Subscribe before taking the snapshot, so nothing that happens in between is lost.
    // An update that arrives twice that way does no harm.
    let live = BroadcastStream::new(state.events.subscribe())
        .filter_map(|event| std::future::ready(event.ok()));
    // A client that connects later still lists the downloads that started before it
    let snapshot = state
        .context
        .download_manager()
        .task_snapshots()
        .into_iter()
        .map(Event::from);

    let mut shutdown = state.shutdown.clone();
    let stream = stream::iter(snapshot)
        .chain(live)
        .filter_map(|event| std::future::ready(sse_event(&event)))
        .map(Ok)
        // An open stream would keep the server from shutting down
        .take_until(async move {
            let _ = shutdown.wait_for(|shutdown| *shutdown).await;
        });

    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn sse_event(event: &Event) -> Option<SseEvent> {
    let payload = event.payload().ok()?;
    SseEvent::default()
        .event(event.name())
        .json_data(payload)
        .ok()
}

#[cfg(test)]
mod tests {
    use hitomi_core::events::DownloadSpeedEvent;

    use super::*;

    #[tokio::test]
    async fn an_event_goes_out_under_the_name_the_frontend_listens_to() {
        use axum::response::IntoResponse;

        let event: Event = DownloadSpeedEvent {
            speed: "1.00 MB/s".to_string(),
        }
        .into();
        let sse = Sse::new(stream::iter([Ok::<_, Infallible>(
            sse_event(&event).unwrap(),
        )]));
        let body = axum::body::to_bytes(sse.into_response().into_body(), 1024)
            .await
            .unwrap();

        assert_eq!(
            std::str::from_utf8(&body).unwrap(),
            "event: download-speed-event\ndata: {\"speed\":\"1.00 MB/s\"}\n\n"
        );
    }

    #[test]
    fn a_completed_download_leaves_the_comic_cache() {
        let comics = Arc::new(ComicCache::default());
        comics.insert(&hitomi_core::types::Comic {
            id: 7,
            ..Default::default()
        });
        let (events, _receiver) = broadcast::channel(8);
        let sink = BroadcastSink {
            events,
            comics: comics.clone(),
        };

        let update = |state| {
            Event::DownloadTask(DownloadTaskEvent::Update {
                comic_id: 7,
                state,
                downloaded_img_count: 1,
                total_img_count: 1,
            })
        };
        sink.emit(update(DownloadTaskState::Downloading));
        assert_eq!(comics.ids(), [7]);
        sink.emit(update(DownloadTaskState::Completed));
        assert!(comics.ids().is_empty());
    }
}
