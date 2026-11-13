//! GStreamer capture: pipelines + encoded-frame fanout. Ported from
//! dataplane/src/media.rs (compile-verified); the broadcast feeds every
//! peer's track writers.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use futures_util::StreamExt;
use gstreamer as gst;
use gstreamer::prelude::*;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

pub const VIDEO_CHANNEL_CAP: usize = 90; // ~3s at 30fps
pub const AUDIO_CHANNEL_CAP: usize = 150;

#[derive(Clone, Debug)]
pub struct EncodedFrame {
    pub data: Bytes,
    pub duration: Duration,
}

/// Owns the capture pipelines and fans encoded frames out to peer track
/// writers via broadcast channels (one per stream). Started on the first
/// peer, stopped when the last peer goes away.
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
    /// `video`/`audio` are full `gst_parse_launch` strings, each containing
    /// `appsink name=appsink`. Empty strings skip the stream.
    pub fn start(video_pipeline: &str, audio_pipeline: &str) -> Result<Option<Arc<Self>>> {
        if video_pipeline.is_empty() && audio_pipeline.is_empty() {
            return Ok(None);
        }

        let (video_tx, _) = broadcast::channel(VIDEO_CHANNEL_CAP);
        let (audio_tx, _) = broadcast::channel(AUDIO_CHANNEL_CAP);

        let mut pipelines = Vec::new();
        if !video_pipeline.is_empty() {
            match Self::launch("video", video_pipeline, video_tx.clone()) {
                Ok(p) => pipelines.push(p),
                Err(err) => error!(?err, "video pipeline failed to start"),
            }
        }
        if !audio_pipeline.is_empty() {
            match Self::launch("audio", audio_pipeline, audio_tx.clone()) {
                Ok(p) => pipelines.push(p),
                Err(err) => error!(?err, "audio pipeline failed to start"),
            }
        }

        if pipelines.is_empty() {
            anyhow::bail!("no capture pipeline could start");
        }

        info!(pipelines = pipelines.len(), "media source started");
        Ok(Some(Arc::new(Self {
            pipelines,
            video_tx,
            audio_tx,
        })))
    }

    fn launch(
        name: &'static str,
        pipeline_str: &str,
        tx: broadcast::Sender<EncodedFrame>,
    ) -> Result<PipelineHandle> {
        info!(%name, %pipeline_str, "creating capture pipeline");

        let pipeline = gst::parse::launch(pipeline_str)
            .with_context(|| format!("parse {name} pipeline"))?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("pipeline is not a gst::Pipeline"))?;

        let appsink = pipeline
            .by_name("appsink")
            .ok_or_else(|| anyhow!("pipeline has no appsink"))?
            .downcast::<gstreamer_app::AppSink>()
            .map_err(|_| anyhow!("appsink is not an AppSink"))?;

        // the live source paces the pipeline; no sink-side clock sync
        appsink.set_property("sync", false);
        appsink.set_property("max-buffers", 10u32);
        appsink.set_property("drop", true);

        appsink.set_callbacks(
            gstreamer_app::AppSinkCallbacks::builder()
                .new_sample(move |appsink| {
                    let sample = appsink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
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

        let bus = pipeline.bus().context("get pipeline bus")?;
        let bus_watch = tokio::spawn(async move {
            use gst::MessageView;
            let mut stream = bus.stream();
            while let Some(msg) = stream.next().await {
                match msg.view() {
                    MessageView::Error(err) => {
                        error!(pipeline = name, error = %err.error(), "gstreamer error");
                    }
                    MessageView::Eos(_) => warn!(pipeline = name, "gstreamer EOS"),
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

    pub fn pipeline_count(&self) -> usize {
        self.pipelines.len()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_audio_device_omits_property() {
        let p = default_audio_pipeline("");
        assert!(!p.contains("device="), "empty device must omit the property: {p}");
        assert!(p.starts_with("pulsesrc !"));
        assert!(p.contains("appsink name=appsink"));
    }

    #[test]
    fn audio_device_set_when_given() {
        let p = default_audio_pipeline("alsa_output.pci-0000");
        assert!(p.starts_with("pulsesrc device=alsa_output.pci-0000 !"));
    }

    #[test]
    fn video_pipeline_layout() {
        let p = default_video_pipeline(":99.0", false, "vp8");
        assert!(p.starts_with("ximagesrc display-name=:99.0 show-pointer=false"));
        assert!(p.contains("vp8enc"));
        assert!(p.ends_with("appsink name=appsink"));
        let h = default_video_pipeline(":0", true, "h264");
        assert!(h.contains("show-pointer=true"));
        assert!(h.contains("x264enc"));
    }
}

/// Assemble the video pipeline string for a video stream (port of the Go
/// capture defaults: ximagesrc + fps caps + vp8enc/x264enc with the same
/// parameter values as server/internal/config).
pub fn default_video_pipeline(display: &str, show_pointer: bool, codec: &str) -> String {
    let fps_pipeline = "capsfilter caps=video/x-raw,framerate=2500/100 name=framerate ! \
                        videoconvert ! queue max-size-buffers=1 max-size-time=0 \
                        max-size-bytes=0 leaky=downstream";
    match codec.to_lowercase().as_str() {
        "h264" => format!(
            "ximagesrc display-name={display} show-pointer={show_pointer} use-damage=false ! \
             {fps_pipeline} ! video/x-raw,format=I420 ! x264enc threads=4 bitrate=4096 \
             key-int-max=15 byte-stream=true tune=zerolatency speed-preset=veryfast ! \
             video/x-h264,stream-format=byte-stream ! appsink name=appsink"
        ),
        // vp8 (default, matches Go codec.VP8 defaults)
        _ => format!(
            "ximagesrc display-name={display} show-pointer={show_pointer} use-damage=false ! \
             {fps_pipeline} ! vp8enc name=encoder target-bitrate=1996800 cpu-used=4 \
             end-usage=cbr threads=4 deadline=1 undershoot=95 buffer-size=12288 \
             buffer-initial-size=6144 buffer-optimal-size=9216 keyframe-max-dist=25 \
             min-quantizer=4 max-quantizer=20 ! appsink name=appsink"
        ),
    }
}

/// Assemble the audio pipeline string (pulsesrc + opusenc, Go defaults).
/// An empty device must OMIT `device=` entirely — `pulsesrc device= ! ...`
/// fails gst-launch parsing ("no element audio") and kills the whole
/// capture start.
pub fn default_audio_pipeline(device: &str) -> String {
    let source = if device.is_empty() {
        "pulsesrc".to_owned()
    } else {
        format!("pulsesrc device={device}")
    };
    format!(
        "{source} ! audio/x-raw,channels=2 ! audioconvert ! \
         queue max-size-buffers=5 leaky=downstream ! opusenc inband-fec=true \
         bitrate=128000 ! appsink name=appsink"
    )
}
