use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use bytes::Bytes;
use futures_util::StreamExt;
use gstreamer as gst;
use gstreamer::prelude::*;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

/// One encoded frame (or audio chunk) out of a GStreamer appsink.
#[derive(Clone, Debug)]
pub struct EncodedFrame {
    pub data: Bytes,
    pub duration: Duration,
}

pub const VIDEO_CHANNEL_CAP: usize = 90; // ~3s at 30fps
pub const AUDIO_CHANNEL_CAP: usize = 150; // ~3s at 50Hz

/// Owns the GStreamer capture pipelines and fans encoded frames out to
/// peer track writers via broadcast channels (one per stream). Started on
/// the first peer, stopped when the last peer goes away — mirroring the
/// embedded implementation, which only encodes while somebody watches.
pub struct MediaSource {
    pipelines: Vec<PipelineHandle>,
    pub video_tx: broadcast::Sender<EncodedFrame>,
    pub audio_tx: broadcast::Sender<EncodedFrame>,
}

struct PipelineHandle {
    pipeline: gst::Pipeline,
    name: &'static str,
    _bus_watch: JoinHandle<()>,
}

impl MediaSource {
    /// `video`/`audio` are full `gst_parse_launch` strings, each
    /// containing `appsink name=appsink`. Empty strings skip the stream.
    pub fn start(video_pipeline: &str, audio_pipeline: &str) -> Result<Option<Arc<Self>>> {
        if video_pipeline.is_empty() && audio_pipeline.is_empty() {
            return Ok(None);
        }

        let (video_tx, _) = broadcast::channel(VIDEO_CHANNEL_CAP);
        let (audio_tx, _) = broadcast::channel(AUDIO_CHANNEL_CAP);

        let mut pipelines = Vec::new();

        if !video_pipeline.is_empty() {
            pipelines.push(Self::launch_pipeline("video", video_pipeline, video_tx.clone())?);
        }

        if !audio_pipeline.is_empty() {
            pipelines.push(Self::launch_pipeline("audio", audio_pipeline, audio_tx.clone())?);
        }

        info!(
            pipelines = pipelines.len(),
            "media source started"
        );

        Ok(Some(Arc::new(Self {
            pipelines,
            video_tx,
            audio_tx,
        })))
    }

    fn launch_pipeline(
        name: &'static str,
        pipeline_str: &str,
        tx: broadcast::Sender<EncodedFrame>,
    ) -> Result<PipelineHandle> {
        info!(%name, %pipeline_str, "creating capture pipeline");

        let pipeline = gst::parse::launch(pipeline_str)
            .with_context(|| format!("parse {name} pipeline"))?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow::anyhow!("pipeline is not a gst::Pipeline"))?;

        let appsink = pipeline
            .by_name("appsink")
            .ok_or_else(|| anyhow::anyhow!("pipeline has no appsink"))?
            .downcast::<gstreamer_app::AppSink>()
            .map_err(|_| anyhow::anyhow!("appsink is not an AppSink"))?;

        // the live source paces the pipeline; no sink-side clock sync
        appsink.set_property("sync", false);
        appsink.set_property("max-buffers", 10u32);
        appsink.set_property("drop", true);

        appsink.set_callbacks(
            gstreamer_app::AppSinkCallbacks::builder()
                .new_sample(move |appsink| {
                    let sample = appsink
                        .pull_sample()
                        .map_err(|_| gst::FlowError::Eos)?;

                    let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                    let duration = buffer
                        .duration()
                        .map(|d| d.into())
                        .unwrap_or_else(|| default_duration(name));

                    let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                    let frame = EncodedFrame {
                        data: Bytes::copy_from_slice(&map),
                        duration,
                    };

                    // no receivers (no peers) is fine — frames are dropped
                    let _ = tx.send(frame);

                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        pipeline
            .set_state(gst::State::Playing)
            .with_context(|| format!("start {name} pipeline"))?;

        // bus watch: log errors, so a broken pipeline is visible in logs
        let bus = pipeline.bus().context("get pipeline bus")?;
        let bus_watch = tokio::spawn(async move {
            use gst::MessageView;
            let mut stream = bus.stream();
            while let Some(msg) = stream.next().await {
                match msg.view() {
                    MessageView::Error(err) => {
                        error!(
                            pipeline = name,
                            error = %err.error(),
                            debug = ?err.debug(),
                            "gstreamer pipeline error"
                        );
                    }
                    MessageView::Eos(_) => {
                        warn!(pipeline = name, "gstreamer pipeline EOS");
                    }
                    MessageView::StateChanged(_) => {}
                    _ => {}
                }
            }
        });

        Ok(PipelineHandle {
            pipeline,
            name,
            _bus_watch: bus_watch,
        })
    }

    pub fn subscribe_video(&self) -> broadcast::Receiver<EncodedFrame> {
        self.video_tx.subscribe()
    }

    pub fn subscribe_audio(&self) -> broadcast::Receiver<EncodedFrame> {
        self.audio_tx.subscribe()
    }
}

impl Drop for MediaSource {
    fn drop(&mut self) {
        for handle in &self.pipelines {
            info!(pipeline = handle.name, "stopping capture pipeline");
            let _ = handle.pipeline.set_state(gst::State::Null);
        }
    }
}

impl Drop for PipelineHandle {
    fn drop(&mut self) {
        self._bus_watch.abort();
    }
}

fn default_duration(name: &str) -> Duration {
    if name == "audio" {
        Duration::from_millis(20)
    } else {
        Duration::from_millis(40)
    }
}
