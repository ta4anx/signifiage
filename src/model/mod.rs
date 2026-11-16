//! Project document model. Everything here is plain data: no GTK, no threads.

pub mod deck;
pub mod layer;
pub mod playlist;
pub mod project;
pub mod prompter;
pub mod queue;
pub mod schedule;
pub mod soundboard;

pub use deck::Deck;
pub use layer::{
    CaptureProps, CaptureSource, ClockPreset, ClockProps, EtaProps, Fit, ImageProps, Layer,
    LayerKind, PatternKind, PatternProps, QueueProps, Rect, Rgba, TextAlign, TextProps, TextStyle,
    TickerProps, TimerMode, TimerProps, VideoProps,
};
pub use playlist::{Playlist, PlaylistItem};
pub use project::{
    Design, LoadedProject, OutputScale, Presentation, Project, Scene, Template, TimerDef,
};
pub use prompter::Prompter;
pub use queue::{QueueData, QueueRow, QueueSource, QueueStatus};
pub use schedule::Schedule;
pub use soundboard::{Pad, Soundboard, SoundboardPage};

/// Fresh unique id for scenes, layers, playlists and rows.
pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Current local time as an RFC 3339 string with offset.
pub fn now_rfc3339() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}
