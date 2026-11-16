//! Soundboard playback engine: one `playbin3` per playing pad.
//!
//! No GTK here. The soundboard page owns an engine, starts pads from clicks and
//! hotkeys, and drains [`SoundboardEngine::poll`] from its 100 ms UI tick.
//!
//! Cover art must never open a window, so every pad pipeline gets a `fakesink`
//! video sink and a `pulsesink` audio sink (the configured PipeWire node when
//! one is selected).

use std::collections::HashMap;
use std::path::PathBuf;

use gstreamer as gst;
use gstreamer::prelude::*;

use crate::model::soundboard::Pad;
use crate::services::media;

struct PadPipeline {
    pipeline: gst::Pipeline,
    playbin: gst::Element,
    looped: bool,
}

/// Active pad pipelines; pads without an asset never get one.
pub struct SoundboardEngine {
    pads: HashMap<String, PadPipeline>,
    master_mute: bool,
    last_error: Option<String>,
}

impl Default for SoundboardEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for SoundboardEngine {
    fn drop(&mut self) {
        for (_, entry) in self.pads.drain() {
            let _ = entry.pipeline.set_state(gst::State::Null);
        }
    }
}

impl SoundboardEngine {
    pub fn new() -> Self {
        Self {
            pads: HashMap::new(),
            master_mute: false,
            last_error: None,
        }
    }

    /// Start `pad`, replacing any pipeline already running for its id.
    ///
    /// Pads without an asset are ignored. Playback failures are reported through
    /// [`Self::take_error`] so the page can show a toast without holding a
    /// second borrow of the engine.
    pub fn play(
        &mut self,
        pad: &Pad,
        base: Option<&PathBuf>,
        sink: Option<&str>,
        master_mute: bool,
    ) {
        let Some(asset) = pad
            .asset
            .as_deref()
            .map(str::trim)
            .filter(|asset| !asset.is_empty())
        else {
            return;
        };
        self.master_mute = master_mute;
        self.stop(&pad.id);
        let Some(path) = media::resolve_asset(base, asset) else {
            self.last_error = Some(format!("no project folder for asset {asset}"));
            return;
        };
        if !path.is_file() {
            self.last_error = Some(format!("audio asset not found: {}", path.display()));
            return;
        }
        let Ok(uri) = media::file_uri(&path) else {
            self.last_error = Some(format!("cannot convert {} to a media URI", path.display()));
            return;
        };
        if let Err(err) = self.start(&pad.id, &uri, pad, sink) {
            self.last_error = Some(err);
        }
    }

    fn start(
        &mut self,
        pad_id: &str,
        uri: &str,
        pad: &Pad,
        sink: Option<&str>,
    ) -> Result<(), String> {
        if let Err(err) = gst::init() {
            return Err(format!("GStreamer could not be initialised: {err}"));
        }
        let playbin = gst::ElementFactory::make("playbin3")
            .build()
            .map_err(|err| format!("playbin3 unavailable: {err}"))?;
        let video_sink = gst::ElementFactory::make("fakesink")
            .build()
            .map_err(|err| format!("fakesink unavailable: {err}"))?;
        playbin.set_property("video-sink", &video_sink);

        let mut sink_error = None;
        match gst::ElementFactory::make("pulsesink").build() {
            Ok(audio_sink) => {
                if let Some(device) = sink.filter(|device| !device.is_empty()) {
                    audio_sink.set_property("device", device);
                }
                playbin.set_property("audio-sink", &audio_sink);
            }
            // Keep playing through playbin's default sink when PulseAudio is
            // absent, but tell the operator why the selected device is unused.
            Err(err) => sink_error = Some(format!("pulsesink unavailable: {err}")),
        }

        playbin.set_property("uri", uri);
        playbin.set_property("volume", pad.volume.clamp(0.0, 1.0));
        playbin.set_property("mute", self.master_mute);

        let pipeline = playbin
            .clone()
            .downcast::<gst::Pipeline>()
            .map_err(|_| "playbin3 is not a pipeline".to_string())?;
        if let Err(err) = pipeline.set_state(gst::State::Playing) {
            let _ = pipeline.set_state(gst::State::Null);
            return Err(format!("cannot start pad playback: {err}"));
        }
        self.pads.insert(
            pad_id.to_string(),
            PadPipeline {
                pipeline,
                playbin,
                looped: pad.r#loop,
            },
        );
        if let Some(message) = sink_error {
            self.last_error = Some(message);
        }
        Ok(())
    }

    pub fn stop(&mut self, pad_id: &str) {
        if let Some(entry) = self.pads.remove(pad_id) {
            let _ = entry.pipeline.set_state(gst::State::Null);
        }
    }

    pub fn stop_all(&mut self) {
        for (_, entry) in self.pads.drain() {
            let _ = entry.pipeline.set_state(gst::State::Null);
        }
    }

    pub fn set_master_mute(&mut self, muted: bool) {
        self.master_mute = muted;
        for entry in self.pads.values() {
            entry.playbin.set_property("mute", muted);
        }
    }

    pub fn is_playing(&self, pad_id: &str) -> bool {
        self.pads.contains_key(pad_id)
    }

    /// Ids of every playing pad, sorted for stable snapshots.
    pub fn playing(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.pads.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Drain bus messages: restart looping pads, stop finished/errored ones.
    /// Returns the ids that stopped during this call.
    pub fn poll(&mut self) -> Vec<String> {
        let mut stopped = Vec::new();
        let mut restarts = Vec::new();
        for (pad_id, entry) in self.pads.iter_mut() {
            let Some(bus) = entry.pipeline.bus() else {
                continue;
            };
            while let Some(message) = bus.pop() {
                use gst::MessageView;
                match message.view() {
                    MessageView::Eos(_) => {
                        if entry.looped {
                            restarts.push(pad_id.clone());
                        } else {
                            stopped.push(pad_id.clone());
                        }
                    }
                    MessageView::Error(err) => {
                        tracing::warn!(
                            pad = %pad_id,
                            error = %err.error(),
                            debug = err.debug().map(|d| d.to_string()).unwrap_or_default(),
                            "soundboard pad error"
                        );
                        self.last_error = Some(err.error().to_string());
                        stopped.push(pad_id.clone());
                    }
                    _ => {}
                }
            }
        }
        for pad_id in restarts {
            let Some(entry) = self.pads.get(&pad_id) else {
                continue;
            };
            let seeked = entry.pipeline.seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                gst::ClockTime::ZERO,
            );
            if seeked.is_ok() {
                let _ = entry.pipeline.set_state(gst::State::Playing);
            } else {
                stopped.push(pad_id);
            }
        }
        stopped.sort();
        stopped.dedup();
        for pad_id in &stopped {
            self.stop(pad_id);
        }
        stopped
    }

    /// Most recent playback failure, consumed once by the UI.
    pub fn take_error(&mut self) -> Option<String> {
        self.last_error.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pads_without_assets_are_ignored() {
        let mut engine = SoundboardEngine::new();
        let mut pad = Pad::default();
        pad.asset = None;
        engine.play(&pad, None, None, false);
        assert!(!engine.is_playing(&pad.id));
        assert!(engine.playing().is_empty());
        assert!(engine.poll().is_empty());
        assert!(engine.take_error().is_none());
    }

    #[test]
    fn missing_project_folder_reports_an_error() {
        let mut engine = SoundboardEngine::new();
        let mut pad = Pad::default();
        pad.asset = Some("assets/beep.wav".to_string());
        engine.play(&pad, None, None, false);
        assert!(!engine.is_playing(&pad.id));
        assert!(engine.take_error().is_some());
        assert!(engine.take_error().is_none());
    }

    #[test]
    fn mute_with_no_pads_is_harmless() {
        let mut engine = SoundboardEngine::new();
        engine.set_master_mute(true);
        assert!(engine.playing().is_empty());
    }
}
