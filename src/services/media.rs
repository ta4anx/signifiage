//! GStreamer media: video layers and capture pipelines.
//!
//! One `MediaHub` per output view. The controller decides *what* should play
//! (`MediaSpec`); the hub keeps pipelines in sync, exposes paintables for
//! rendering and reports EOS/errors via `poll`, drained by the UI tick.
//!
//! Pipeline descriptions are pure helpers so tests can substitute `appsink`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gstreamer as gst;
use gstreamer::prelude::*;
use gtk::gdk;
use gtk::glib;
use gtk4 as gtk;

/// What a layer should play.
#[derive(Debug, Clone, PartialEq)]
pub enum MediaSource {
    Video {
        uri: String,
        /// Restart from the beginning on EOS instead of stopping.
        repeat: bool,
        volume: f64,
    },
    Capture {
        description: String,
        sink_name: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct MediaSpec {
    /// Layer id: the key the renderer asks a paintable for.
    pub layer_id: String,
    /// Identity of the source; a change rebuilds the pipeline.
    pub key: String,
    pub source: MediaSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaEvent {
    Eos { layer_id: String },
    Error { layer_id: String, message: String },
}

struct LayerPipeline {
    pipeline: gst::Pipeline,
    spec: MediaSpec,
    paintable: Option<gdk::Paintable>,
    audio_sink: Option<gst::Element>,
    playbin: Option<gst::Element>,
}

/// Pipeline factory descriptions, parameterised by sink element name.
pub fn camera_description(
    device: &str,
    width: Option<u32>,
    height: Option<u32>,
    fps: Option<u32>,
    sink: &str,
) -> String {
    let mut caps = String::new();
    if let (Some(w), Some(h)) = (width, height)
        && w > 0
        && h > 0
    {
        let framerate = fps
            .filter(|f| *f > 0)
            .map(|f| format!(",framerate={f}/1"))
            .unwrap_or_default();
        caps = format!(" ! video/x-raw,width={w},height={h}{framerate}");
    }
    format!(
        "v4l2src device={device} ! videoconvert{caps} ! videoscale ! videoconvert ! {sink} name=sink"
    )
}

pub fn x11_description(xid: u32, sink: &str) -> String {
    format!(
        "ximagesrc xid={xid} use-damage=false show-pointer=false ! videoconvert ! {sink} name=sink"
    )
}

pub fn portal_description(fd: i32, node: u32, sink: &str) -> String {
    format!("pipewiresrc fd={fd} path={node} do-timestamp=true ! videoconvert ! {sink} name=sink")
}

/// Description for the audio test tone (1 s sine).
pub fn test_tone_description(device: Option<&str>) -> String {
    match device.filter(|d| !d.is_empty()) {
        Some(device) => format!(
            "audiotestsrc num-buffers=50 wave=sine ! audioconvert ! pulsesink device={device}"
        ),
        None => "audiotestsrc num-buffers=50 wave=sine ! audioconvert ! pulsesink".to_string(),
    }
}

pub struct MediaHub {
    pipelines: HashMap<String, LayerPipeline>,
    sink_device: Option<String>,
    muted: bool,
    paused: bool,
    init_error: Option<String>,
    errors: Vec<(String, String)>,
}

impl Default for MediaHub {
    fn default() -> Self {
        Self::new()
    }
}

impl MediaHub {
    /// Initialise GStreamer; failures surface through `init_error`.
    pub fn new() -> Self {
        let init_error = match gst::init() {
            Ok(()) => None,
            Err(err) => Some(err.to_string()),
        };
        Self {
            pipelines: HashMap::new(),
            sink_device: None,
            muted: false,
            paused: false,
            init_error,
            errors: Vec::new(),
        }
    }

    pub fn available(&self) -> bool {
        self.init_error.is_none()
    }

    pub fn init_error(&self) -> Option<&str> {
        self.init_error.as_deref()
    }

    /// Changing the sink takes effect on the next media start.
    pub fn set_sink(&mut self, device: Option<String>) {
        if self.sink_device != device {
            self.sink_device = device;
            self.stop_all();
        }
    }

    pub fn sink(&self) -> Option<&str> {
        self.sink_device.as_deref()
    }

    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        for entry in self.pipelines.values() {
            if let Some(playbin) = &entry.playbin {
                playbin.set_property("mute", muted);
            }
            if let Some(sink) = &entry.audio_sink {
                sink.set_property("mute", muted);
            }
        }
    }

    /// Freeze support: pipelines pause while the captured texture is shown.
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        for entry in self.pipelines.values() {
            let _ = entry.pipeline.set_state(if paused {
                gst::State::Paused
            } else {
                gst::State::Playing
            });
        }
    }

    pub fn stop_all(&mut self) {
        for (_, entry) in self.pipelines.drain() {
            let _ = entry.pipeline.set_state(gst::State::Null);
        }
    }

    pub fn stop_layer(&mut self, layer_id: &str) {
        if let Some(entry) = self.pipelines.remove(layer_id) {
            let _ = entry.pipeline.set_state(gst::State::Null);
        }
    }

    pub fn paintable(&self, layer_id: &str) -> Option<gdk::Paintable> {
        self.pipelines
            .get(layer_id)
            .and_then(|entry| entry.paintable.clone())
    }

    pub fn playing_layers(&self) -> Vec<String> {
        let mut layers: Vec<String> = self.pipelines.keys().cloned().collect();
        layers.sort();
        layers
    }

    pub fn is_empty(&self) -> bool {
        self.pipelines.is_empty()
    }

    /// Make live pipelines match `specs`; `audio` is false for muted views.
    pub fn reconcile(&mut self, specs: &[MediaSpec], audio: bool) {
        if !self.available() {
            self.stop_all();
            return;
        }
        let wanted: HashMap<&str, &MediaSpec> = specs
            .iter()
            .map(|spec| (spec.layer_id.as_str(), spec))
            .collect();

        let stale: Vec<String> = self
            .pipelines
            .iter()
            .filter(|(id, entry)| match wanted.get(id.as_str()) {
                Some(spec) => spec.key != entry.spec.key,
                None => true,
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            self.stop_layer(&id);
        }

        for spec in specs {
            if let Some(entry) = self.pipelines.get(&spec.layer_id) {
                apply_audio(entry, audio, self.muted);
                continue;
            }
            match self.start(spec, audio) {
                Ok(entry) => {
                    self.pipelines.insert(spec.layer_id.clone(), entry);
                }
                Err(err) => {
                    tracing::warn!(layer = %spec.layer_id, %err, "media pipeline failed");
                    self.errors.push((spec.layer_id.clone(), err));
                }
            }
        }
    }

    fn start(&self, spec: &MediaSpec, audio: bool) -> Result<LayerPipeline, String> {
        match &spec.source {
            MediaSource::Video {
                uri,
                repeat,
                volume,
            } => {
                let playbin = gst::ElementFactory::make("playbin3")
                    .build()
                    .map_err(|err| format!("playbin3 unavailable: {err}"))?;
                let video_sink = gst::ElementFactory::make("gtk4paintablesink")
                    .build()
                    .map_err(|err| format!("gtk4paintablesink unavailable: {err}"))?;
                let audio_sink = gst::ElementFactory::make("pulsesink")
                    .build()
                    .map_err(|err| format!("pulsesink unavailable: {err}"))?;
                if let Some(device) = &self.sink_device {
                    audio_sink.set_property("device", device);
                }
                playbin.set_property("video-sink", &video_sink);
                playbin.set_property("audio-sink", &audio_sink);
                playbin.set_property("uri", uri.as_str());
                playbin.set_property("volume", *volume);
                playbin.set_property("mute", !audio || self.muted);
                let _ = repeat;
                let pipeline = playbin
                    .clone()
                    .downcast::<gst::Pipeline>()
                    .map_err(|_| "playbin3 is not a pipeline".to_string())?;
                pipeline
                    .set_state(if self.paused {
                        gst::State::Paused
                    } else {
                        gst::State::Playing
                    })
                    .map_err(|err| format!("cannot start playback: {err}"))?;
                Ok(LayerPipeline {
                    pipeline,
                    spec: spec.clone(),
                    paintable: sink_paintable(&video_sink),
                    audio_sink: Some(audio_sink),
                    playbin: Some(playbin),
                })
            }
            MediaSource::Capture {
                description,
                sink_name,
            } => {
                let element = gst::parse::launch(description)
                    .map_err(|err| format!("cannot build capture pipeline: {err}"))?;
                let pipeline = element
                    .clone()
                    .downcast::<gst::Pipeline>()
                    .map_err(|_| "capture description is not a pipeline".to_string())?;
                let sink = pipeline
                    .by_name(sink_name)
                    .ok_or_else(|| format!("capture sink {sink_name} missing"))?;
                pipeline
                    .set_state(if self.paused {
                        gst::State::Paused
                    } else {
                        gst::State::Playing
                    })
                    .map_err(|err| format!("cannot start capture: {err}"))?;
                Ok(LayerPipeline {
                    pipeline,
                    spec: spec.clone(),
                    paintable: sink_paintable(&sink),
                    audio_sink: None,
                    playbin: None,
                })
            }
        }
    }

    /// Drain bus messages; call from the UI tick.
    pub fn poll(&mut self) -> Vec<MediaEvent> {
        let mut events = Vec::new();
        let mut restarts = Vec::new();
        for (layer_id, entry) in self.pipelines.iter_mut() {
            let bus = entry.pipeline.bus();
            let Some(bus) = bus else { continue };
            while let Some(message) = bus.pop() {
                use gst::MessageView;
                match message.view() {
                    MessageView::Eos(_) => {
                        if matches!(entry.spec.source, MediaSource::Video { repeat: true, .. }) {
                            restarts.push(layer_id.clone());
                        } else {
                            let _ = entry.pipeline.set_state(gst::State::Paused);
                            events.push(MediaEvent::Eos {
                                layer_id: layer_id.clone(),
                            });
                        }
                    }
                    MessageView::Error(err) => {
                        let message = format!(
                            "{} ({})",
                            err.error(),
                            err.debug().map(|d| d.to_string()).unwrap_or_default()
                        );
                        tracing::warn!(layer = %layer_id, %message, "media error");
                        let _ = entry.pipeline.set_state(gst::State::Null);
                        events.push(MediaEvent::Error {
                            layer_id: layer_id.clone(),
                            message,
                        });
                    }
                    _ => {}
                }
            }
        }
        for layer_id in restarts {
            if let Some(entry) = self.pipelines.get(&layer_id) {
                let _ = entry
                    .pipeline
                    .seek_simple(gst::SeekFlags::FLUSH, gst::ClockTime::ZERO);
                let _ = entry.pipeline.set_state(gst::State::Playing);
            }
        }
        events
    }

    /// Errors captured while starting pipelines since the last call.
    pub fn take_errors(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.errors)
    }

    /// Audio sinks that expose a PipeWire `node.name`, deduped and sorted.
    pub fn audio_devices() -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        if gst::init().is_err() {
            return out;
        }
        let monitor = gst::DeviceMonitor::new();
        monitor.add_filter(Some("Audio/Sink"), None);
        if monitor.start().is_err() {
            return out;
        }
        for device in monitor.devices() {
            let Some(node) = device
                .properties()
                .as_ref()
                .and_then(|props| props.get::<String>("node.name").ok())
            else {
                continue;
            };
            if out.iter().any(|(existing, _)| existing == &node) {
                continue;
            }
            out.push((node, device.display_name().to_string()));
        }
        monitor.stop();
        out.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
        out
    }

    /// One-second sine through the configured sink.
    pub fn play_test_tone(device: Option<&str>) -> Result<(), String> {
        let description = test_tone_description(device);
        let pipeline = gst::parse::launch(&description)
            .map_err(|err| format!("cannot build test tone: {err}"))?;
        let pipeline = pipeline
            .downcast::<gst::Pipeline>()
            .map_err(|_| "test tone is not a pipeline".to_string())?;
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|err| format!("cannot start test tone: {err}"))?;
        std::thread::sleep(std::time::Duration::from_millis(1200));
        let _ = pipeline.set_state(gst::State::Null);
        Ok(())
    }
}

impl Drop for MediaHub {
    fn drop(&mut self) {
        self.stop_all();
    }
}

fn apply_audio(entry: &LayerPipeline, audio: bool, muted: bool) {
    if let Some(playbin) = &entry.playbin {
        playbin.set_property("mute", !audio || muted);
    }
    if let Some(sink) = &entry.audio_sink {
        sink.set_property("mute", !audio || muted);
    }
}

fn sink_paintable(sink: &gst::Element) -> Option<gdk::Paintable> {
    sink.property::<Option<gdk::Paintable>>("paintable")
}

/// Convert a file path into a playbin URI.
pub fn file_uri(path: &Path) -> Result<String, String> {
    glib::filename_to_uri(path, None)
        .map(|uri| uri.to_string())
        .map_err(|err| format!("cannot convert {} to a URI: {err}", path.display()))
}

/// Resolve a stored relative asset path inside a project directory.
pub fn resolve_asset(base: Option<&PathBuf>, asset: &str) -> Option<PathBuf> {
    let path = PathBuf::from(asset);
    if path.is_absolute() {
        return Some(path);
    }
    base.map(|base| base.join(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_description_includes_optional_caps() {
        let plain = camera_description("/dev/video0", None, None, None, "appsink");
        assert!(plain.starts_with("v4l2src device=/dev/video0"));
        assert!(plain.ends_with("appsink name=sink"));
        assert!(!plain.contains("video/x-raw"));

        let sized = camera_description("/dev/video0", Some(1280), Some(720), Some(30), "appsink");
        assert!(sized.contains("video/x-raw,width=1280,height=720,framerate=30/1"));
    }

    #[test]
    fn x11_and_portal_descriptions_are_parameterised() {
        let x11 = x11_description(1234, "appsink");
        assert!(x11.contains("xid=1234"));
        assert!(x11.contains("use-damage=false"));
        let portal = portal_description(7, 42, "appsink");
        assert!(portal.contains("fd=7"));
        assert!(portal.contains("path=42"));
        assert!(portal.contains("do-timestamp=true"));
    }

    #[test]
    fn capture_pipelines_parse_with_appsink() {
        if gst::init().is_err() {
            return;
        }
        // Only the camera pipeline is safe to instantiate in a test process:
        // `ximagesrc` opens the display immediately and aborts on an invalid xid.
        let description = camera_description("/dev/video0", None, None, None, "appsink");
        let parsed = gst::parse::launch(&description);
        assert!(parsed.is_ok(), "{description}: {parsed:?}");
        if let Ok(element) = parsed {
            let _ = element.set_state(gst::State::Null);
        }
    }

    #[test]
    fn test_tone_description_uses_the_selected_device() {
        assert_eq!(
            test_tone_description(None),
            "audiotestsrc num-buffers=50 wave=sine ! audioconvert ! pulsesink"
        );
        assert!(test_tone_description(Some("alsa_output.x")).contains("device=alsa_output.x"));
    }

    #[test]
    fn file_uri_converts_paths() {
        let uri = file_uri(Path::new("/tmp/beep.wav")).unwrap();
        assert!(uri.starts_with("file:///tmp/beep.wav"), "{uri}");
    }

    #[test]
    fn resolve_asset_joins_the_project_base() {
        let base = PathBuf::from("/tmp/show");
        assert_eq!(
            resolve_asset(Some(&base), "assets/clip.mp4"),
            Some(PathBuf::from("/tmp/show/assets/clip.mp4"))
        );
        assert_eq!(
            resolve_asset(Some(&base), "/abs/clip.mp4"),
            Some(PathBuf::from("/abs/clip.mp4"))
        );
        assert_eq!(resolve_asset(None, "assets/clip.mp4"), None);
    }
}
