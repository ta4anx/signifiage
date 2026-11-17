//! Call chime: a bundled "ding-dong" plus an optional user-selected file.
//!
//! The WAV is embedded in the binary, written to the cache directory on first
//! use and played through `playbin3` so it shares the audio path with everything
//! else. Failures are logged, never fatal: a broken sound must not stop a call.

use std::path::{Path, PathBuf};

use gstreamer as gst;
use gstreamer::prelude::*;

/// Bundled default chime (44.1 kHz, mono, ~1 s).
const BUNDLED: &[u8] = include_bytes!("../../data/queue_chime.wav");

/// Cache file used for the bundled chime.
pub fn bundled_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".cache")))?;
    Some(base.join("signifiage").join("queue_chime.wav"))
}

/// Write the bundled chime once; returns the file to play.
pub fn ensure_bundled_file() -> Option<PathBuf> {
    let path = bundled_path()?;
    let up_to_date = std::fs::metadata(&path)
        .map(|meta| meta.len() == BUNDLED.len() as u64)
        .unwrap_or(false);
    if !up_to_date {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok()?;
        }
        std::fs::write(&path, BUNDLED).ok()?;
    }
    Some(path)
}

/// Play a one-shot preview on a worker thread (used by the settings test button).
pub fn play_once(path: &Path, device: Option<String>) {
    let path = path.to_path_buf();
    let spawned = std::thread::Builder::new()
        .name("signifiage-chime-preview".to_string())
        .spawn(move || {
            let mut player = ChimePlayer::new();
            player.play(&path, device.as_deref());
            std::thread::sleep(std::time::Duration::from_millis(1600));
            player.stop();
        });
    if let Err(err) = spawned {
        tracing::warn!("cannot start the chime preview: {err}");
    }
}

/// Resolve which file a call should play, honouring the user's choice.
pub fn resolve(enabled: bool, custom: Option<&Path>) -> Option<PathBuf> {
    if !enabled {
        return None;
    }
    match custom {
        Some(path) if path.is_file() => Some(path.to_path_buf()),
        Some(_) => {
            tracing::warn!("configured call sound is missing; falling back to the bundled chime");
            ensure_bundled_file()
        }
        None => ensure_bundled_file(),
    }
}

/// One-shot chime player. Replaces the previous pipeline on every call.
#[derive(Default)]
pub struct ChimePlayer {
    pipeline: Option<gst::Pipeline>,
}

impl ChimePlayer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Play `path`; EOS or error clears the pipeline.
    pub fn play(&mut self, path: &Path, device: Option<&str>) {
        self.stop();
        if gst::init().is_err() {
            return;
        }
        let playbin = match gst::ElementFactory::make("playbin3").build() {
            Ok(element) => element,
            Err(err) => {
                tracing::warn!("call chime unavailable: {err}");
                return;
            }
        };
        let sink = match gst::ElementFactory::make("pulsesink").build() {
            Ok(element) => element,
            Err(err) => {
                tracing::warn!("call chime has no audio sink: {err}");
                return;
            }
        };
        if let Some(device) = device {
            sink.set_property("device", device);
        }
        let uri = match gtk4::glib::filename_to_uri(path, None) {
            Ok(uri) => uri.to_string(),
            Err(err) => {
                tracing::warn!("call chime path is not playable: {err}");
                return;
            }
        };
        // Video streams must not open a sink for a sound file with cover art.
        if let Ok(fakesink) = gst::ElementFactory::make("fakesink").build() {
            playbin.set_property("video-sink", &fakesink);
        }
        playbin.set_property("audio-sink", &sink);
        playbin.set_property("uri", uri.as_str());
        let pipeline = match playbin.downcast::<gst::Pipeline>() {
            Ok(pipeline) => pipeline,
            Err(_) => {
                tracing::warn!("call chime playbin is not a pipeline");
                return;
            }
        };
        if pipeline.set_state(gst::State::Playing).is_err() {
            tracing::warn!("call chime could not start");
            return;
        }
        self.pipeline = Some(pipeline);
    }

    pub fn stop(&mut self) {
        if let Some(pipeline) = self.pipeline.take() {
            let _ = pipeline.set_state(gst::State::Null);
        }
    }

    /// Drain the bus; call from the UI tick.
    pub fn poll(&mut self) {
        let Some(pipeline) = self.pipeline.as_ref() else {
            return;
        };
        let Some(bus) = pipeline.bus() else {
            return;
        };
        while let Some(message) = bus.pop() {
            use gst::MessageView;
            match message.view() {
                MessageView::Eos(_) | MessageView::Error(_) => {
                    self.pipeline = None;
                    break;
                }
                _ => {}
            }
        }
    }
}

impl Drop for ChimePlayer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_chime_is_a_wav() {
        assert_eq!(&BUNDLED[0..4], b"RIFF");
        assert_eq!(&BUNDLED[8..12], b"WAVE");
        assert!(BUNDLED.len() > 40_000);
    }

    #[test]
    fn disabled_sound_resolves_to_nothing() {
        assert_eq!(resolve(false, None), None);
    }

    #[test]
    fn missing_custom_sound_falls_back_to_the_bundled_file() {
        let resolved = resolve(true, Some(Path::new("/definitely/not/here.wav")));
        assert!(resolved.is_some_and(|path| path.ends_with("queue_chime.wav")));
    }
}
