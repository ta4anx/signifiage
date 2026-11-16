//! Web remote actions: wire strings in, typed commands out.
//!
//! Parsing is pure so it can be unit-tested without a server, a runtime or a
//! display. Unknown actions and missing/invalid arguments are rejected here;
//! semantics (what happens when the queue is empty, for example) belong to the
//! UI thread that drains the channel.

use std::fmt;

use serde_json::{Map, Value};

/// A command for the queue service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueCommand {
    Next,
    Recall,
    Done,
    Skip,
    Undo,
    Add { name: String, note: String },
}

/// A transport / program command.
#[derive(Debug, Clone, PartialEq)]
pub enum PlaybackCommand {
    Next,
    Prev,
    Go,
    Black { on: bool },
    Freeze { on: bool },
    Mute { on: bool },
    Playlist { id: String },
    Auto { on: bool },
}

/// A timer command; ids that no longer exist are no-ops on the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimerCommand {
    Start { id: String },
    Pause { id: String },
    Reset { id: String },
}

/// Teleprompter controls.
#[derive(Debug, Clone, PartialEq)]
pub enum PrompterCommand {
    Start,
    Pause,
    Reset,
    Speed { value: f64 },
}

/// Soundboard controls; pads are addressed by id across all pages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoundboardCommand {
    Play { pad_id: String },
    Stop { pad_id: String },
    StopAll,
    Page { index: usize },
}

/// Keys sent to the focused presentation application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeckCommand {
    Next,
    Prev,
}

/// Built-in test patterns and resuming the interrupted program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternCommand {
    Show { id: String },
    Resume,
}

/// Everything the web remote can ask the application to do.
#[derive(Debug, Clone, PartialEq)]
pub enum RemoteAction {
    Queue(QueueCommand),
    Playback(PlaybackCommand),
    Timer(TimerCommand),
    Prompter(PrompterCommand),
    Soundboard(SoundboardCommand),
    Deck(DeckCommand),
    Pattern(PatternCommand),
    /// Internal notification: the upload endpoint stored `assets/<name>`.
    ///
    /// Clients cannot trigger this through `/api/actions`; it exists so the
    /// desktop asset list refreshes after a web upload.
    AssetUploaded {
        asset: String,
    },
}

impl RemoteAction {
    /// Wire name of the action, for logging and diagnostics.
    pub fn name(&self) -> &'static str {
        match self {
            RemoteAction::Queue(command) => match command {
                QueueCommand::Next => "queue.next",
                QueueCommand::Recall => "queue.recall",
                QueueCommand::Done => "queue.done",
                QueueCommand::Skip => "queue.skip",
                QueueCommand::Undo => "queue.undo",
                QueueCommand::Add { .. } => "queue.add",
            },
            RemoteAction::Playback(command) => match command {
                PlaybackCommand::Next => "playback.next",
                PlaybackCommand::Prev => "playback.prev",
                PlaybackCommand::Go => "playback.go",
                PlaybackCommand::Black { .. } => "playback.black",
                PlaybackCommand::Freeze { .. } => "playback.freeze",
                PlaybackCommand::Mute { .. } => "playback.mute",
                PlaybackCommand::Playlist { .. } => "playback.playlist",
                PlaybackCommand::Auto { .. } => "playback.auto",
            },
            RemoteAction::Timer(command) => match command {
                TimerCommand::Start { .. } => "timer.start",
                TimerCommand::Pause { .. } => "timer.pause",
                TimerCommand::Reset { .. } => "timer.reset",
            },
            RemoteAction::Prompter(command) => match command {
                PrompterCommand::Start => "prompter.start",
                PrompterCommand::Pause => "prompter.pause",
                PrompterCommand::Reset => "prompter.reset",
                PrompterCommand::Speed { .. } => "prompter.speed",
            },
            RemoteAction::Soundboard(command) => match command {
                SoundboardCommand::Play { .. } => "soundboard.play",
                SoundboardCommand::Stop { .. } => "soundboard.stop",
                SoundboardCommand::StopAll => "soundboard.stop_all",
                SoundboardCommand::Page { .. } => "soundboard.page",
            },
            RemoteAction::Deck(command) => match command {
                DeckCommand::Next => "deck.next",
                DeckCommand::Prev => "deck.prev",
            },
            RemoteAction::Pattern(command) => match command {
                PatternCommand::Show { .. } => "pattern.show",
                PatternCommand::Resume => "pattern.resume",
            },
            RemoteAction::AssetUploaded { .. } => "asset.uploaded",
        }
    }
}

/// Why a request was rejected (mapped to HTTP 400 by the route).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionError {
    message: String,
}

impl ActionError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ActionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ActionError {}

/// Parse one `{action, args}` request into a typed command.
pub fn parse_action(action: &str, args: &Value) -> Result<RemoteAction, ActionError> {
    let empty = Map::new();
    let args = match args {
        Value::Object(map) => map,
        Value::Null => &empty,
        _ => return Err(ActionError::new("args must be a JSON object")),
    };

    match action {
        "queue.next" => Ok(RemoteAction::Queue(QueueCommand::Next)),
        "queue.recall" => Ok(RemoteAction::Queue(QueueCommand::Recall)),
        "queue.done" => Ok(RemoteAction::Queue(QueueCommand::Done)),
        "queue.skip" => Ok(RemoteAction::Queue(QueueCommand::Skip)),
        "queue.undo" => Ok(RemoteAction::Queue(QueueCommand::Undo)),
        "queue.add" => Ok(RemoteAction::Queue(QueueCommand::Add {
            name: require_name(args, "name")?,
            note: optional_string(args, "note")?.unwrap_or_default(),
        })),

        "playback.next" => Ok(RemoteAction::Playback(PlaybackCommand::Next)),
        "playback.prev" => Ok(RemoteAction::Playback(PlaybackCommand::Prev)),
        "playback.go" => Ok(RemoteAction::Playback(PlaybackCommand::Go)),
        "playback.black" => Ok(RemoteAction::Playback(PlaybackCommand::Black {
            on: require_bool(args, "on")?,
        })),
        "playback.freeze" => Ok(RemoteAction::Playback(PlaybackCommand::Freeze {
            on: require_bool(args, "on")?,
        })),
        "playback.mute" => Ok(RemoteAction::Playback(PlaybackCommand::Mute {
            on: require_bool(args, "on")?,
        })),
        "playback.playlist" => Ok(RemoteAction::Playback(PlaybackCommand::Playlist {
            id: require_id(args, "id")?,
        })),
        "playback.auto" => Ok(RemoteAction::Playback(PlaybackCommand::Auto {
            on: require_bool(args, "on")?,
        })),

        "timer.start" => Ok(RemoteAction::Timer(TimerCommand::Start {
            id: require_id(args, "id")?,
        })),
        "timer.pause" => Ok(RemoteAction::Timer(TimerCommand::Pause {
            id: require_id(args, "id")?,
        })),
        "timer.reset" => Ok(RemoteAction::Timer(TimerCommand::Reset {
            id: require_id(args, "id")?,
        })),

        "prompter.start" => Ok(RemoteAction::Prompter(PrompterCommand::Start)),
        "prompter.pause" => Ok(RemoteAction::Prompter(PrompterCommand::Pause)),
        "prompter.reset" => Ok(RemoteAction::Prompter(PrompterCommand::Reset)),
        "prompter.speed" => Ok(RemoteAction::Prompter(PrompterCommand::Speed {
            value: require_number(args, "value")?,
        })),

        "soundboard.play" => Ok(RemoteAction::Soundboard(SoundboardCommand::Play {
            pad_id: require_id(args, "pad_id")?,
        })),
        "soundboard.stop" => Ok(RemoteAction::Soundboard(SoundboardCommand::Stop {
            pad_id: require_id(args, "pad_id")?,
        })),
        "soundboard.stop_all" => Ok(RemoteAction::Soundboard(SoundboardCommand::StopAll)),
        "soundboard.page" => Ok(RemoteAction::Soundboard(SoundboardCommand::Page {
            index: require_index(args, "index")?,
        })),

        "deck.next" => Ok(RemoteAction::Deck(DeckCommand::Next)),
        "deck.prev" => Ok(RemoteAction::Deck(DeckCommand::Prev)),

        "pattern.show" => Ok(RemoteAction::Pattern(PatternCommand::Show {
            id: require_id(args, "id")?,
        })),
        "pattern.resume" => Ok(RemoteAction::Pattern(PatternCommand::Resume)),

        // Not reachable from the network: uploads are the only producer.
        "asset.uploaded" => Err(ActionError::new("unknown action")),
        other => Err(ActionError::new(format!("unknown action {other:?}"))),
    }
}

fn require_bool(args: &Map<String, Value>, key: &str) -> Result<bool, ActionError> {
    match args.get(key) {
        Some(Value::Bool(value)) => Ok(*value),
        _ => Err(ActionError::new(format!(
            "missing or invalid boolean argument {key:?}"
        ))),
    }
}

fn require_number(args: &Map<String, Value>, key: &str) -> Result<f64, ActionError> {
    let value = args
        .get(key)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .ok_or_else(|| ActionError::new(format!("missing or invalid number argument {key:?}")))?;
    Ok(value)
}

fn require_index(args: &Map<String, Value>, key: &str) -> Result<usize, ActionError> {
    let value = args
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| ActionError::new(format!("missing or invalid index argument {key:?}")))?;
    usize::try_from(value)
        .map_err(|_| ActionError::new(format!("index argument {key:?} is out of range")))
}

fn require_id(args: &Map<String, Value>, key: &str) -> Result<String, ActionError> {
    let value = args
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ActionError::new(format!("missing or invalid id argument {key:?}")))?;
    Ok(value.to_string())
}

fn require_name(args: &Map<String, Value>, key: &str) -> Result<String, ActionError> {
    let value = args
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ActionError::new(format!("missing or invalid name argument {key:?}")))?;
    Ok(value.to_string())
}

fn optional_string(args: &Map<String, Value>, key: &str) -> Result<Option<String>, ActionError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.trim().to_string())),
        Some(_) => Err(ActionError::new(format!(
            "argument {key:?} must be a string"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(action: &str, args: Value) -> RemoteAction {
        parse_action(action, &args).expect("action should parse")
    }

    #[test]
    fn simple_actions_map_to_their_commands() {
        let cases = [
            ("queue.next", RemoteAction::Queue(QueueCommand::Next)),
            ("queue.recall", RemoteAction::Queue(QueueCommand::Recall)),
            ("queue.done", RemoteAction::Queue(QueueCommand::Done)),
            ("queue.skip", RemoteAction::Queue(QueueCommand::Skip)),
            ("queue.undo", RemoteAction::Queue(QueueCommand::Undo)),
            (
                "playback.next",
                RemoteAction::Playback(PlaybackCommand::Next),
            ),
            (
                "playback.prev",
                RemoteAction::Playback(PlaybackCommand::Prev),
            ),
            ("playback.go", RemoteAction::Playback(PlaybackCommand::Go)),
            (
                "prompter.start",
                RemoteAction::Prompter(PrompterCommand::Start),
            ),
            (
                "prompter.pause",
                RemoteAction::Prompter(PrompterCommand::Pause),
            ),
            (
                "prompter.reset",
                RemoteAction::Prompter(PrompterCommand::Reset),
            ),
            (
                "soundboard.stop_all",
                RemoteAction::Soundboard(SoundboardCommand::StopAll),
            ),
            ("deck.next", RemoteAction::Deck(DeckCommand::Next)),
            ("deck.prev", RemoteAction::Deck(DeckCommand::Prev)),
            (
                "pattern.resume",
                RemoteAction::Pattern(PatternCommand::Resume),
            ),
        ];
        for (name, expected) in cases {
            assert_eq!(parse(name, json!({})), expected, "action {name}");
        }
    }

    #[test]
    fn toggles_carry_their_boolean() {
        assert_eq!(
            parse("playback.black", json!({"on": true})),
            RemoteAction::Playback(PlaybackCommand::Black { on: true })
        );
        assert_eq!(
            parse("playback.freeze", json!({"on": false})),
            RemoteAction::Playback(PlaybackCommand::Freeze { on: false })
        );
        assert_eq!(
            parse("playback.mute", json!({"on": true})),
            RemoteAction::Playback(PlaybackCommand::Mute { on: true })
        );
        assert_eq!(
            parse("playback.auto", json!({"on": false})),
            RemoteAction::Playback(PlaybackCommand::Auto { on: false })
        );
    }

    #[test]
    fn arguments_are_required_where_the_plan_says_so() {
        assert!(parse_action("playback.black", &json!({})).is_err());
        assert!(parse_action("playback.black", &json!({"on": "yes"})).is_err());
        assert!(parse_action("timer.start", &json!({})).is_err());
        assert!(parse_action("timer.pause", &json!({"id": "  "})).is_err());
        assert!(parse_action("timer.reset", &json!({"id": 7})).is_err());
        assert!(parse_action("soundboard.play", &json!({})).is_err());
        assert!(parse_action("soundboard.page", &json!({"index": -1})).is_err());
        assert!(parse_action("soundboard.page", &json!({"index": 1.5})).is_err());
        assert!(parse_action("playback.playlist", &json!({})).is_err());
        assert!(parse_action("pattern.show", &json!({"id": ""})).is_err());
        assert!(parse_action("prompter.speed", &json!({})).is_err());
        assert!(parse_action("prompter.speed", &json!({"value": "fast"})).is_err());
    }

    #[test]
    fn queue_add_defaults_the_note_and_requires_a_name() {
        assert_eq!(
            parse("queue.add", json!({"name": " Ayşe "})),
            RemoteAction::Queue(QueueCommand::Add {
                name: "Ayşe".to_string(),
                note: String::new(),
            })
        );
        assert_eq!(
            parse("queue.add", json!({"name": "Mehmet", "note": "Kontrol"})),
            RemoteAction::Queue(QueueCommand::Add {
                name: "Mehmet".to_string(),
                note: "Kontrol".to_string(),
            })
        );
        assert!(parse_action("queue.add", &json!({})).is_err());
        assert!(parse_action("queue.add", &json!({"name": "   "})).is_err());
        assert!(parse_action("queue.add", &json!({"name": "x", "note": 3})).is_err());
    }

    #[test]
    fn ids_and_indices_are_preserved() {
        assert_eq!(
            parse("timer.start", json!({"id": "show"})),
            RemoteAction::Timer(TimerCommand::Start {
                id: "show".to_string()
            })
        );
        assert_eq!(
            parse("soundboard.play", json!({"pad_id": "pad-1"})),
            RemoteAction::Soundboard(SoundboardCommand::Play {
                pad_id: "pad-1".to_string()
            })
        );
        assert_eq!(
            parse("soundboard.page", json!({"index": 2})),
            RemoteAction::Soundboard(SoundboardCommand::Page { index: 2 })
        );
        assert_eq!(
            parse("prompter.speed", json!({"value": 60})),
            RemoteAction::Prompter(PrompterCommand::Speed { value: 60.0 })
        );
    }

    #[test]
    fn unknown_actions_and_bad_args_are_errors() {
        assert!(parse_action("queue.explode", &json!({})).is_err());
        assert!(parse_action("", &json!({})).is_err());
        assert!(parse_action("queue.next", &json!("nope")).is_err());
        // Internal upload notification is not a client action.
        assert!(parse_action("asset.uploaded", &json!({"asset": "assets/x.png"})).is_err());
    }

    #[test]
    fn names_cover_every_variant() {
        let actions = [
            RemoteAction::Queue(QueueCommand::Add {
                name: "x".into(),
                note: String::new(),
            }),
            RemoteAction::Playback(PlaybackCommand::Playlist { id: "p".into() }),
            RemoteAction::Timer(TimerCommand::Start { id: "t".into() }),
            RemoteAction::Prompter(PrompterCommand::Speed { value: 45.0 }),
            RemoteAction::Soundboard(SoundboardCommand::Page { index: 0 }),
            RemoteAction::Deck(DeckCommand::Prev),
            RemoteAction::Pattern(PatternCommand::Show { id: "focus".into() }),
            RemoteAction::AssetUploaded {
                asset: "assets/x.png".into(),
            },
        ];
        for action in actions {
            assert!(!action.name().is_empty());
        }
    }
}
