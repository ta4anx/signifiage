//! Bridge between the controller and the LAN web remote.
//!
//! Owns the `RemoteServer`, builds the `RemoteState` snapshot from the controller
//! (plus the runtime flags pages publish), drains incoming actions on the UI tick
//! and restarts the server when the configuration changes.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use crate::config::Config;
use crate::model::layer::PatternKind;
use crate::server::{
    DeckInfo, Mode as ServerMode, PadInfo, PatternInfo, PlaylistInfo, PlaylistRef, ProgramInfo,
    PrompterInfo, QueueEntryInfo, QueueInfo, RemoteAction, RemoteServer, RemoteState,
    SoundboardInfo, SoundboardPageInfo, TimerInfo,
};
use crate::services::controller::{
    Controller, DeckCall, Mode, ProgramSource, PrompterCall, SoundboardCall,
};

/// Republish at least this often so timer/queue text stays fresh.
const REFRESH: Duration = Duration::from_millis(500);

/// What the settings page shows about the server.
#[derive(Debug, Clone, Default)]
pub struct RemoteStatus {
    pub running: bool,
    pub url: Option<String>,
    pub error: Option<String>,
}

pub struct RemoteBridge {
    controller: Rc<Controller>,
    server: RefCell<Option<RemoteServer>>,
    receiver: RefCell<Option<mpsc::Receiver<RemoteAction>>>,
    last_error: RefCell<Option<String>>,
    started: Cell<(u16, bool)>,
    last_publish: Cell<Option<Instant>>,
}

impl RemoteBridge {
    pub fn new(controller: Rc<Controller>) -> Rc<Self> {
        Rc::new(Self {
            controller,
            server: RefCell::new(None),
            receiver: RefCell::new(None),
            last_error: RefCell::new(None),
            started: Cell::new((0, false)),
            last_publish: Cell::new(None),
        })
    }

    pub fn status(&self) -> RemoteStatus {
        let running = self.server.borrow().is_some();
        RemoteStatus {
            running,
            url: self
                .server
                .borrow()
                .as_ref()
                .map(|server| server.local_url()),
            error: self.last_error.borrow().clone(),
        }
    }

    /// Start, restart or stop the server so it matches `config`.
    pub fn apply_config(&self, config: &Config) {
        let wanted = config.remote.enabled;
        let port = config.remote.port;
        let (current_port, current_enabled) = self.started.get();
        if current_enabled == wanted && (!wanted || current_port == port) {
            return;
        }
        self.stop();
        if !wanted {
            self.started.set((port, false));
            tracing::info!("web remote disabled");
            return;
        }
        let token = config.remote.token.trim().to_string();
        if token.is_empty() {
            self.last_error
                .replace(Some("web remote has no token".to_string()));
            self.started.set((port, false));
            tracing::warn!("web remote not started: empty token");
            return;
        }
        let (tx, rx) = mpsc::channel(256);
        let mode = match self.controller.mode() {
            Mode::Gui => ServerMode::Gui,
            Mode::Player => ServerMode::Player,
        };
        match RemoteServer::start(port, token, mode, tx) {
            Ok(server) => {
                if let Some(dir) = self.controller.project_dir() {
                    server.set_project_dir(Some(dir));
                }
                tracing::info!("web remote listening on {}", server.local_url());
                *self.server.borrow_mut() = Some(server);
                *self.receiver.borrow_mut() = Some(rx);
                self.last_error.replace(None);
                self.started.set((port, true));
                self.publish();
            }
            Err(err) => {
                tracing::warn!("web remote could not start on port {port}: {err}");
                self.last_error.replace(Some(err.to_string()));
                self.started.set((port, false));
            }
        }
    }

    pub fn stop(&self) {
        if let Some(server) = self.server.borrow_mut().take() {
            server.stop();
        }
        self.receiver.borrow_mut().take();
        *self.receiver.borrow_mut() = None;
    }

    pub fn shutdown(&self) {
        self.stop();
        self.started.set((0, false));
    }

    /// UI tick: apply queued remote actions and publish fresh state.
    pub fn tick(&self) {
        let actions: Vec<RemoteAction> = {
            let mut receiver = self.receiver.borrow_mut();
            let Some(receiver) = receiver.as_mut() else {
                return;
            };
            let mut actions = Vec::new();
            loop {
                match receiver.try_recv() {
                    Ok(action) => actions.push(action),
                    Err(_) => break,
                }
            }
            actions
        };
        let had_actions = !actions.is_empty();
        for action in actions {
            self.apply_action(action);
        }
        let due = self
            .last_publish
            .get()
            .map(|at| at.elapsed() >= REFRESH)
            .unwrap_or(true);
        if had_actions || due {
            self.publish();
        }
    }

    fn apply_action(&self, action: RemoteAction) {
        use crate::server::{PatternCommand, PlaybackCommand, QueueCommand, TimerCommand};
        match action {
            RemoteAction::Queue(command) => match command {
                QueueCommand::Next => {
                    self.controller.queue_next();
                }
                QueueCommand::Recall => {
                    self.controller.queue_recall();
                }
                QueueCommand::Done => {
                    self.controller.queue_done();
                }
                QueueCommand::Skip => {
                    self.controller.queue_skip();
                }
                QueueCommand::Undo => {
                    self.controller.queue_undo();
                }
                QueueCommand::Add { name, note } => {
                    if !self.controller.queue_add(&name, &note) {
                        tracing::warn!("remote queue add rejected for empty name");
                    }
                }
            },
            RemoteAction::Playback(command) => match command {
                PlaybackCommand::Next | PlaybackCommand::Go => {
                    self.controller.go();
                }
                PlaybackCommand::Prev => {
                    self.controller.prev();
                }
                PlaybackCommand::Black { on } => self.controller.set_black(on),
                PlaybackCommand::Freeze { on } => self.controller.set_frozen(on),
                PlaybackCommand::Mute { on } => self.controller.set_muted(on),
                PlaybackCommand::Playlist { id } => {
                    if !self.controller.play_playlist(&id) {
                        tracing::warn!("remote playlist {id} is not playable");
                    }
                }
                PlaybackCommand::Auto { on } => self.controller.set_auto(on),
            },
            RemoteAction::Timer(command) => match command {
                TimerCommand::Start { id } => {
                    self.controller.timer_start(&id);
                }
                TimerCommand::Pause { id } => {
                    self.controller.timer_pause(&id);
                }
                TimerCommand::Reset { id } => {
                    self.controller.timer_reset(&id);
                }
            },
            RemoteAction::Prompter(command) => {
                let call = match command {
                    crate::server::PrompterCommand::Start => PrompterCall::Start,
                    crate::server::PrompterCommand::Pause => PrompterCall::Pause,
                    crate::server::PrompterCommand::Reset => PrompterCall::Reset,
                    crate::server::PrompterCommand::Speed { value } => PrompterCall::Speed(value),
                };
                self.controller.prompter_call(call);
            }
            RemoteAction::Soundboard(command) => {
                let call = match command {
                    crate::server::SoundboardCommand::Play { pad_id } => {
                        SoundboardCall::Play(pad_id)
                    }
                    crate::server::SoundboardCommand::Stop { pad_id } => {
                        SoundboardCall::Stop(pad_id)
                    }
                    crate::server::SoundboardCommand::StopAll => SoundboardCall::StopAll,
                    crate::server::SoundboardCommand::Page { index } => SoundboardCall::Page(index),
                };
                self.controller.soundboard_call(call);
            }
            RemoteAction::Deck(command) => {
                let call = match command {
                    crate::server::DeckCommand::Next => DeckCall::Next,
                    crate::server::DeckCommand::Prev => DeckCall::Prev,
                };
                self.controller.deck_call(call);
            }
            RemoteAction::Pattern(PatternCommand::Show { id }) => {
                if let Some(kind) = PatternKind::from_id(&id) {
                    self.controller.show_pattern(kind);
                }
            }
            RemoteAction::Pattern(PatternCommand::Resume) => self.controller.resume_program(),
            RemoteAction::AssetUploaded { asset } => {
                tracing::info!("remote uploaded {asset}");
                self.controller.mark_assets_changed();
                self.controller
                    .toast(&rust_i18n::t!("assets.imported", count = 1).to_string());
            }
        }
    }

    /// Build and send a state snapshot.
    pub fn publish(&self) {
        let Some(state) = self.build_state() else {
            return;
        };
        if let Some(server) = self.server.borrow().as_ref() {
            // Projects can be opened after the server starts (and closed again):
            // uploads need the current directory before the next request.
            server.set_project_dir(self.controller.project_dir());
            server.publish(&state);
        }
        self.last_publish.set(Some(Instant::now()));
    }

    fn build_state(&self) -> Option<RemoteState> {
        let mode = match self.controller.mode() {
            Mode::Gui => ServerMode::Gui,
            Mode::Player => ServerMode::Player,
        };
        let mut state = RemoteState::new(mode, crate::i18n::remote_locale());
        state.project = self
            .controller
            .is_open()
            .then(|| self.controller.project_name());

        let program = self.controller.program();
        state.program = ProgramInfo {
            name: self.controller.program_name(),
            scene_id: program.scene_id.clone(),
            source: program.source.kind_id().to_string(),
            black: self.controller.is_black(),
            freeze: self.controller.is_frozen(),
            mute: self.controller.is_muted(),
            on_air: !program.is_black(),
            remaining_secs: self.controller.program_remaining(),
        };
        state.pattern_override = program.source.is_pattern()
            || program
                .saved
                .as_ref()
                .is_some_and(|_| matches!(program.source, ProgramSource::Pattern { .. }));

        let preview = self.controller.preview_state();
        let playlists = self.controller.playlists();
        if let Some(playlist) = self.controller.preview_playlist() {
            state.playlist = PlaylistInfo {
                id: playlist.id.clone(),
                name: playlist.name.clone(),
                preview_index: preview.index,
                program_index: program.source.playlist_index(),
                total: playlist.items.len(),
                looping: playlist.r#loop,
                auto: self.controller.auto_advance(),
            };
        }
        state.playlists = playlists
            .into_iter()
            .map(|(id, name)| PlaylistRef { id, name })
            .collect();

        let queue = self.controller.queue();
        let entry = |row: &crate::model::QueueRow| QueueEntryInfo {
            ticket: row.ticket.clone(),
            name: row.name.clone(),
            note: row.note.clone(),
        };
        state.queue = QueueInfo {
            current: queue.current.as_ref().map(entry),
            next: queue
                .upcoming
                .iter()
                .take(5)
                .map(|row| entry(row))
                .collect(),
            waiting: queue.waiting,
            called: queue.called,
            done: queue.done,
            skipped: queue.skipped,
        };

        state.timers = self
            .controller
            .timer_snapshots()
            .into_iter()
            .map(|snapshot| TimerInfo {
                id: snapshot.id,
                name: snapshot.name,
                running: snapshot.running,
                elapsed_secs: snapshot.elapsed_secs,
                remaining_secs: snapshot.remaining_secs,
                end_at: snapshot.end_at.map(crate::services::timers::format_end_at),
            })
            .collect();

        let live = self.controller.live();
        state.prompter = PrompterInfo {
            running: live.prompter_running.get(),
            speed: live.prompter_speed.get(),
        };

        let project = self.controller.project_clone();
        let playing = live.playing_pads.borrow().clone();
        state.soundboard = SoundboardInfo {
            active_page: live.soundboard_page.get(),
            pages: project
                .soundboard
                .pages
                .iter()
                .map(|page| SoundboardPageInfo {
                    id: page.id.clone(),
                    name: page.name.clone(),
                    pads: page
                        .pads
                        .iter()
                        .map(|pad| PadInfo {
                            id: pad.id.clone(),
                            label: pad.label.clone(),
                            color: pad.color.clone(),
                            hotkey: pad.hotkey.clone().unwrap_or_default(),
                            playing: playing.iter().any(|id| id == &pad.id),
                        })
                        .collect(),
                })
                .collect(),
        };

        state.deck = DeckInfo {
            available: live.deck_available.get(),
            label: rust_i18n::t!("deck.title").to_string(),
            backend: live.deck_backend.borrow().clone(),
            hint: live.deck_hint.borrow().clone(),
            last_error: live.deck_error.borrow().clone(),
        };

        state.patterns = PatternKind::ALL
            .iter()
            .map(|kind| PatternInfo {
                id: kind.id().to_string(),
                name: rust_i18n::t!(kind.label_key()).to_string(),
            })
            .collect();

        Some(state)
    }
}

impl Drop for RemoteBridge {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Convenience for pages that want the localized deck/backend label.
pub fn backend_label(backend: crate::services::deck::DeckBackend) -> &'static str {
    backend.id()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_is_off_before_start() {
        let controller = Controller::new(Config::default(), Mode::Gui);
        let bridge = RemoteBridge::new(controller);
        assert!(!bridge.status().running);
        assert!(bridge.status().url.is_none());
    }
}
