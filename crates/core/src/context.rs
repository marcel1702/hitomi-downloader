use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use anyhow::{anyhow, Context as _};
use parking_lot::RwLock;

use crate::{
    config::Config,
    download_manager::DownloadManager,
    events::{Event, EventSink},
    hitomi_client::HitomiClient,
};

/// Everything the core needs at runtime: the config, the clients, where its files live,
/// and where its events go
///
/// It takes the place of Tauri's app handle and state, so the core runs the same in the
/// desktop app and on a server. Cloning it only increases a reference count.
#[derive(Clone)]
pub struct Context {
    inner: Arc<Inner>,
}

/// What a context needs beyond where it keeps its files
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Where the pages of a download are kept until the comic is packed into its archive
    ///
    /// Without one the pages go into a `.downloading-<name>` folder next to the comic, as the
    /// desktop app does. With one, no page ever lies in the library: the web version keeps
    /// them in its config volume, so a scan of the library can not find a half finished
    /// download, and a network share only gets the finished archive written once.
    pub temp_dir: Option<PathBuf>,
}

struct Inner {
    data_dir: PathBuf,
    temp_dir: Option<PathBuf>,
    runtime: tokio::runtime::Handle,
    events: Box<dyn EventSink>,
    config: RwLock<Config>,
    // Set right after the context exists, both of them hold a clone of it
    hitomi_client: OnceLock<HitomiClient>,
    download_manager: OnceLock<DownloadManager>,
}

/// The context of this process, for the code that has no context passed in
static GLOBAL: OnceLock<Context> = OnceLock::new();

impl Context {
    /// Create the context of this process, loading the config from `data_dir`
    ///
    /// `data_dir` holds `config.json` and the logs. Background work is spawned onto
    /// `runtime`, events are handed to `events`. There can only be one context per process.
    pub fn init(
        data_dir: PathBuf,
        runtime: tokio::runtime::Handle,
        events: impl EventSink + 'static,
    ) -> anyhow::Result<Context> {
        Context::init_with(data_dir, runtime, events, Options::default())
    }

    /// Like [`Context::init`], with the `options` of this process
    pub fn init_with(
        data_dir: PathBuf,
        runtime: tokio::runtime::Handle,
        events: impl EventSink + 'static,
        options: Options,
    ) -> anyhow::Result<Context> {
        std::fs::create_dir_all(&data_dir).context(format!(
            "create data directory `{}` failed",
            data_dir.display()
        ))?;
        let config = Config::new(&data_dir)?;

        let context = Context {
            inner: Arc::new(Inner {
                data_dir,
                temp_dir: options.temp_dir,
                runtime,
                events: Box::new(events),
                config: RwLock::new(config),
                hitomi_client: OnceLock::new(),
                download_manager: OnceLock::new(),
            }),
        };
        GLOBAL
            .set(context.clone())
            .map_err(|_| anyhow!("the context was already created"))?;

        let hitomi_client = HitomiClient::new(context.clone());
        let _ = context.inner.hitomi_client.set(hitomi_client);
        let download_manager = DownloadManager::new(&context);
        let _ = context.inner.download_manager.set(download_manager);

        Ok(context)
    }

    /// The context of this process
    ///
    /// # Panics
    /// If [`Context::init`] was not called yet.
    pub fn global() -> Context {
        GLOBAL.get().expect("Context not initialized").clone()
    }

    /// Where `config.json` and the logs live
    pub fn data_dir(&self) -> &Path {
        &self.inner.data_dir
    }

    /// Where the pages of a download are kept, if not next to the comic, see [`Options`]
    pub fn temp_dir(&self) -> Option<&Path> {
        self.inner.temp_dir.as_deref()
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.inner.data_dir.join("logs")
    }

    pub fn config(&self) -> &RwLock<Config> {
        &self.inner.config
    }

    pub fn hitomi_client(&self) -> &HitomiClient {
        self.inner
            .hitomi_client
            .get()
            .expect("set by Context::init")
    }

    pub fn download_manager(&self) -> &DownloadManager {
        self.inner
            .download_manager
            .get()
            .expect("set by Context::init")
    }

    /// Send `event` to the frontend
    pub fn emit(&self, event: impl Into<Event>) {
        self.inner.events.emit(event.into());
    }

    /// Run `future` in the background
    ///
    /// Works from any thread, also from the main thread of the desktop app, which is
    /// outside the async runtime.
    pub fn spawn<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.inner.runtime.spawn(future);
    }
}
