//! Standalone signage player: package loading, program output, schedules, queue
//! persistence and the process lifetime of the show.
//!
//! The player owns exactly one program window and one [`Controller`] in
//! [`Mode::Player`]. It never writes the project it plays: queue mutations go to
//! a per-source state file under the XDG state directory.

use std::cell::{Cell, RefCell};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use serde::{Deserialize, Serialize};

use crate::cli::PlayerArgs;
use crate::config::{self, CaptureBackend, Config};
use crate::model::QueueRow;
use crate::model::layer::{CaptureSource, LayerKind, PatternKind};
use crate::model::project;
use crate::model::queue::QueueData;
use crate::server::{
    DeckCommand, PatternCommand, PlaybackCommand, QueueCommand, RemoteAction, RemoteServer,
    RemoteState, SoundboardCommand, TimerCommand,
};
use crate::services::controller::{Controller, Mode, ViewRole};
use crate::services::deck::{DeckBackend, DeckController};
use crate::services::schedule::ScheduleRuntime;
use crate::services::timers;
use crate::services::x11;
use crate::ui::outputs;
use crate::ui::render::{SceneView, ViewMode};

/// Prefix of the temporary extraction directories owned by the player.
const TEMP_PREFIX: &str = "signifiage-player-";
/// Stale extraction directories older than this are removed at startup.
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
/// Windowed program output size when fullscreen is not requested.
const WINDOWED_SIZE: (i32, i32) = (960, 540);
/// Queue state writes are debounced after the last mutation.
const QUEUE_SAVE_DEBOUNCE: Duration = Duration::from_millis(500);
/// UI heartbeat: media bus, auto-advance, render data.
const TICK: Duration = Duration::from_millis(100);
/// Schedule re-evaluation interval.
const SCHEDULE_POLL_SECS: u32 = 30;
/// On-disk queue state schema.
const QUEUE_STATE_FORMAT: u32 = 1;

/// Run the player. Returns the process exit code.
pub fn run(args: &PlayerArgs, config: Config) -> glib::ExitCode {
    cleanup_stale_player_dirs();

    let source = args.path.clone();
    let metadata = match fs::metadata(&source) {
        Ok(metadata) => metadata,
        Err(err) => {
            eprintln!(
                "{}",
                rust_i18n::t!(
                    "player.error.missing_project",
                    path = source.display().to_string(),
                    error = err.to_string()
                )
            );
            return glib::ExitCode::FAILURE;
        }
    };
    let is_package = metadata.is_file();
    if !is_package && !metadata.is_dir() {
        eprintln!(
            "{}",
            rust_i18n::t!(
                "player.error.unsupported_path",
                path = source.display().to_string()
            )
        );
        return glib::ExitCode::FAILURE;
    }

    let canonical = source.canonicalize().unwrap_or_else(|_| source.clone());
    let package_stamp = if is_package {
        package_stamp(&canonical)
    } else {
        None
    };
    let state_key = queue_state::state_key(&canonical, package_stamp);

    let mut transient: Option<PathBuf> = None;
    let project_dir = if is_package {
        let dest = std::env::temp_dir().join(format!("{TEMP_PREFIX}{}", uuid::Uuid::new_v4()));
        if let Err(err) = project::extract_package(&source, &dest) {
            eprintln!(
                "{}",
                rust_i18n::t!(
                    "player.error.extract",
                    path = source.display().to_string(),
                    error = err.to_string()
                )
            );
            remove_dir(&dest);
            return glib::ExitCode::FAILURE;
        }
        transient = Some(dest.clone());
        dest
    } else {
        canonical.clone()
    };

    if let Err(err) = project::load(&project_dir) {
        eprintln!(
            "{}",
            rust_i18n::t!(
                "player.error.invalid_project",
                path = source.display().to_string(),
                error = err.to_string()
            )
        );
        if let Some(dir) = &transient {
            remove_dir(dir);
        }
        return glib::ExitCode::FAILURE;
    }

    let mut config = config;
    if args.remote {
        config.remote.enabled = true;
    }
    if let Some(port) = args.port {
        config.remote.port = port;
    }
    let token_override = args.token.clone().filter(|token| !token.trim().is_empty());
    if let Some(token) = &token_override {
        config.remote.token = token.clone();
    }
    if config.remote.enabled {
        // Generates and persists a token when missing; a player never runs with
        // an unauthenticated remote.
        config.ensure_remote_token();
        if args.port.is_some() || token_override.is_some() {
            if let Err(err) = config.save() {
                tracing::warn!("cannot persist remote overrides: {err}");
            }
        }
    }

    if let Err(err) = gtk::init() {
        eprintln!(
            "{}",
            rust_i18n::t!("player.error.no_display", error = err.to_string())
        );
        if let Some(dir) = &transient {
            remove_dir(dir);
        }
        return glib::ExitCode::FAILURE;
    }

    let boot = Rc::new(Boot {
        dir: project_dir,
        transient,
        state_key,
        config,
        windowed: args.windowed,
        monitor: args.monitor,
    });
    let session: Rc<RefCell<Option<Rc<PlayerSession>>>> = Rc::new(RefCell::new(None));
    let healthy = Rc::new(Cell::new(true));

    let app = gtk::Application::builder()
        .application_id(crate::PLAYER_APP_ID)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();

    {
        let boot = boot.clone();
        let session = session.clone();
        let healthy = healthy.clone();
        app.connect_activate(move |app| {
            if session.borrow().is_some() {
                return;
            }
            match PlayerSession::build(app, &boot) {
                Ok(player) => *session.borrow_mut() = Some(player),
                Err(err) => {
                    eprintln!("{err}");
                    healthy.set(false);
                    app.quit();
                }
            }
        });
    }

    let status = app.run_with_args::<&str>(&[]);

    if let Some(player) = session.borrow_mut().take() {
        player.shutdown();
    }
    if let Some(dir) = &boot.transient {
        remove_dir(dir);
    }
    if !healthy.get() {
        return glib::ExitCode::FAILURE;
    }
    status
}

/// Packaging: save the project synchronously, then write the distribution
/// package. Returns the absolute output path.
pub fn publish(project_dir: &Path, output: &Path) -> anyhow::Result<PathBuf> {
    if !project_dir.is_dir() {
        anyhow::bail!(
            "{}",
            rust_i18n::t!(
                "publish.error.not_a_project",
                path = project_dir.display().to_string()
            )
        );
    }
    let loaded = project::load(project_dir)?;
    for warning in &loaded.warnings {
        tracing::warn!("publish: {warning}");
    }
    project::save(&loaded.project, project_dir)?;
    let path = project::export_package(project_dir, output)?;
    tracing::info!("published {} -> {}", project_dir.display(), path.display());
    Ok(path)
}

/// Inputs captured before the GTK application is built.
struct Boot {
    dir: PathBuf,
    /// Extracted package directory owned by this process (removed on exit).
    transient: Option<PathBuf>,
    state_key: String,
    config: Config,
    windowed: bool,
    monitor: Option<u32>,
}

/// One running player: window, controller, schedule and queue persistence.
struct PlayerSession {
    app: gtk::Application,
    window: gtk::Window,
    controller: Rc<Controller>,
    project_dir: PathBuf,
    schedule: RefCell<ScheduleRuntime>,
    queue: RefCell<QueueSync>,
    remote: RefCell<Option<RemoteServer>>,
    remote_rx: RefCell<Option<tokio::sync::mpsc::Receiver<RemoteAction>>>,
    soundboard: RefCell<crate::services::soundboard::SoundboardEngine>,
    deck: DeckController,
    last_publish: Cell<Instant>,
    inhibit: Cell<Option<u32>>,
}

impl PlayerSession {
    fn build(app: &gtk::Application, boot: &Boot) -> anyhow::Result<Rc<Self>> {
        let window = gtk::Window::builder()
            .application(app)
            .decorated(false)
            .deletable(false)
            .resizable(true)
            .build();
        let title = rust_i18n::t!("player.window.title").to_string();
        window.set_title(Some(&title));
        window.set_default_size(WINDOWED_SIZE.0, WINDOWED_SIZE.1);
        window.set_cursor_from_name(Some("none"));

        let view = SceneView::new(ViewMode::Program);
        view.set_can_focus(false);
        view.set_cursor_from_name(Some("none"));
        window.set_child(Some(&view));

        let controller = Controller::new(boot.config.clone(), Mode::Player);
        controller.open_project(&boot.dir)?;
        if boot.transient.is_some() {
            // `open_project` records the project as recent; a temporary package
            // extraction must never leak into the operator's recent list.
            controller.set_config(boot.config.clone());
            if let Err(err) = boot.config.save() {
                tracing::warn!("cannot restore config after opening a package: {err}");
            }
        }
        controller.attach_view(ViewRole::Program, &view);
        controller.refresh_views();

        let state_path = config::state_dir().join(format!("player-{}.json", boot.state_key));
        let mut data = controller.queue_data();
        if let Some(persisted) = queue_state::read_state(&state_path) {
            data.rows = persisted.rows;
            data.next_ticket = data.next_ticket.max(persisted.next_ticket);
            data.normalize();
            controller.queue_replace(data.clone());
        }
        let queue = QueueSync::new(state_path, &data);

        let (remote, remote_rx) = if boot.config.remote.enabled {
            start_remote(&boot.config)
        } else {
            (None, None)
        };

        let session = Rc::new(Self {
            app: app.clone(),
            window: window.clone(),
            controller: controller.clone(),
            project_dir: boot.dir.clone(),
            schedule: RefCell::new(ScheduleRuntime::new()),
            queue: RefCell::new(queue),
            remote: RefCell::new(remote),
            remote_rx: RefCell::new(remote_rx),
            soundboard: RefCell::new(crate::services::soundboard::SoundboardEngine::new()),
            deck: deck_controller(&boot.config),
            last_publish: Cell::new(Instant::now() - Duration::from_secs(1)),
            inhibit: Cell::new(None),
        });

        session.place_window(boot);
        window.present();
        let cookie = app.inhibit(
            Some(&window),
            gtk::ApplicationInhibitFlags::IDLE,
            Some("Signifiage signage player"),
        );
        session.inhibit.set(Some(cookie));

        session.install_input();
        session.start_show()?;
        session.publish_remote_state();

        {
            let weak = Rc::downgrade(&session);
            glib::timeout_add_local(TICK, move || {
                if let Some(session) = weak.upgrade() {
                    session.tick();
                }
                glib::ControlFlow::Continue
            });
        }
        {
            let weak = Rc::downgrade(&session);
            glib::timeout_add_seconds_local(SCHEDULE_POLL_SECS, move || {
                if let Some(session) = weak.upgrade() {
                    session.poll_schedule();
                }
                glib::ControlFlow::Continue
            });
        }
        Ok(session)
    }

    /// Fullscreen on the requested/configured monitor, or a 960x540 window.
    fn place_window(&self, boot: &Boot) {
        let monitors = outputs::monitor_list();
        let configured = boot
            .monitor
            .map(|index| index as usize)
            .filter(|index| *index < monitors.len())
            .or_else(|| {
                outputs::resolve_monitor(boot.config.outputs.program.monitor.as_ref(), &monitors)
            })
            .unwrap_or(0);
        let windowed = boot.windowed || boot.config.windowed_outputs;
        if windowed {
            self.window
                .set_default_size(WINDOWED_SIZE.0, WINDOWED_SIZE.1);
            return;
        }
        if let Some(monitor) = monitors.get(configured) {
            self.window.fullscreen_on_monitor(monitor);
        }
    }

    /// Keyboard transport. No editable widgets exist in the player, so every
    /// shortcut is safe.
    fn install_input(self: &Rc<Self>) {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, keyval, _keycode, state| {
            let Some(session) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            session.handle_key(keyval, state)
        });
        self.window.add_controller(keys);
    }

    fn handle_key(
        &self,
        keyval: gtk::gdk::Key,
        state: gtk::gdk::ModifierType,
    ) -> glib::Propagation {
        use gtk::gdk::Key;
        if state.contains(gtk::gdk::ModifierType::CONTROL_MASK) && matches!(keyval, Key::q | Key::Q)
        {
            self.quit();
            return glib::Propagation::Stop;
        }
        match keyval {
            Key::space => {
                self.controller.go();
            }
            Key::BackSpace => {
                self.controller.prev();
            }
            Key::b | Key::B => {
                let black = !self.controller.is_black();
                self.controller.set_black(black);
            }
            Key::f | Key::F => {
                let frozen = !self.controller.is_frozen();
                self.controller.set_frozen(frozen);
            }
            Key::m | Key::M => {
                let muted = !self.controller.is_muted();
                self.controller.set_muted(muted);
            }
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    }

    /// Start the schedule-matching playlist (or the project default).
    fn start_show(&self) -> anyhow::Result<()> {
        let project = self.controller.project_clone();
        let decision = self
            .schedule
            .borrow_mut()
            .poll(&project, chrono::Local::now());
        let Some(playlist_id) = decision else {
            anyhow::bail!(
                "{}",
                rust_i18n::t!("player.error.no_playlist", project = project.name)
            );
        };
        if !self.controller.play_playlist(&playlist_id) {
            anyhow::bail!(
                "{}",
                rust_i18n::t!("player.error.no_playlist", project = project.name)
            );
        }
        self.controller.refresh_views();
        Ok(())
    }

    /// Switch playlists when the active schedule window changes.
    fn poll_schedule(&self) {
        let project = self.controller.project_clone();
        let decision = self
            .schedule
            .borrow_mut()
            .poll(&project, chrono::Local::now());
        let Some(playlist_id) = decision else {
            return;
        };
        if self.controller.play_playlist(&playlist_id) {
            tracing::info!("schedule switched to playlist {playlist_id}");
            return;
        }
        tracing::warn!("schedule playlist {playlist_id} is not playable; keeping the default");
        if let Some(fallback) = ScheduleRuntime::current_playlist(&project, chrono::Local::now()) {
            self.controller.play_playlist(&fallback);
        }
    }

    /// 100 ms heartbeat: remote actions, controller (media bus, auto-advance,
    /// 4 fps text), soundboard bus and queue-state persistence.
    fn tick(&self) {
        self.drain_remote();
        self.controller.tick();
        let stopped = self.soundboard.borrow_mut().poll();
        if !stopped.is_empty() {
            tracing::debug!(pads = ?stopped, "soundboard pads stopped");
        }
        if let Some(err) = self.soundboard.borrow_mut().take_error() {
            tracing::warn!("soundboard: {err}");
        }
        let data = self.controller.queue_data();
        {
            let mut queue = self.queue.borrow_mut();
            queue.observe(&data);
            queue.flush_if_due();
        }
        self.publish_remote_state_if_due();
    }

    /// Apply every action the web remote queued since the last tick.
    fn drain_remote(&self) {
        loop {
            let action = {
                let mut slot = self.remote_rx.borrow_mut();
                let Some(receiver) = slot.as_mut() else {
                    return;
                };
                match receiver.try_recv() {
                    Ok(action) => action,
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => return,
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                        *slot = None;
                        return;
                    }
                }
            };
            self.apply_action(action);
        }
    }

    fn apply_action(&self, action: RemoteAction) {
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
                    self.controller.queue_add(&name, &note);
                }
            },
            RemoteAction::Playback(command) => match command {
                PlaybackCommand::Next | PlaybackCommand::Go => {
                    self.controller.go();
                }
                PlaybackCommand::Prev => {
                    self.controller.prev();
                }
                PlaybackCommand::Black { on } => {
                    self.controller.set_black(on);
                }
                PlaybackCommand::Freeze { on } => {
                    self.controller.set_frozen(on);
                }
                PlaybackCommand::Mute { on } => {
                    self.controller.set_muted(on);
                    self.soundboard.borrow_mut().set_master_mute(on);
                }
                PlaybackCommand::Playlist { id } => {
                    if !self.controller.play_playlist(&id) {
                        tracing::warn!("remote asked for unknown or empty playlist {id}");
                    }
                }
                PlaybackCommand::Auto { on } => {
                    self.controller.set_auto(on);
                }
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
            RemoteAction::Pattern(command) => match command {
                PatternCommand::Show { id } => match PatternKind::from_id(&id) {
                    Some(kind) => self.controller.show_pattern(kind),
                    None => tracing::warn!("remote asked for unknown pattern {id}"),
                },
                PatternCommand::Resume => self.controller.resume_program(),
            },
            RemoteAction::Soundboard(command) => self.apply_soundboard(command),
            RemoteAction::Deck(command) => self.apply_deck(command),
            RemoteAction::Prompter(command) => {
                // The player has no prompter output; the desktop GUI owns it.
                tracing::debug!(?command, "prompter action ignored in player mode");
            }
            RemoteAction::AssetUploaded { asset } => {
                // Uploads are rejected in player mode; keep the log honest.
                tracing::warn!("ignoring upload notification for {asset} in player mode");
            }
        }
        self.publish_remote_state();
    }

    fn apply_soundboard(&self, command: SoundboardCommand) {
        match command {
            SoundboardCommand::Play { pad_id } => {
                let project = self.controller.project_clone();
                let Some(pad) = project.soundboard.pad(&pad_id).cloned() else {
                    tracing::warn!("remote asked for unknown soundboard pad {pad_id}");
                    return;
                };
                let base = self.project_dir.clone();
                let sink = self.controller.config().audio_sink;
                let muted = self.controller.is_muted();
                self.soundboard
                    .borrow_mut()
                    .play(&pad, Some(&base), sink.as_deref(), muted);
            }
            SoundboardCommand::Stop { pad_id } => {
                self.soundboard.borrow_mut().stop(&pad_id);
            }
            SoundboardCommand::StopAll => {
                self.soundboard.borrow_mut().stop_all();
            }
            SoundboardCommand::Page { index } => {
                let pages = self
                    .controller
                    .with_project(|project| project.soundboard.pages.len());
                if index < pages {
                    self.controller.mutate_quiet(|project| {
                        project.soundboard.active_page = index;
                    });
                }
            }
        }
    }

    fn apply_deck(&self, command: DeckCommand) {
        let project = self.controller.project_clone();
        let target = self.controller.program().scene.as_ref().and_then(|scene| {
            scene.layers.iter().find_map(|layer| match &layer.kind {
                LayerKind::Capture(props) => match &props.source {
                    CaptureSource::X11Window { xid, .. } if *xid != 0 => Some(*xid),
                    _ => None,
                },
                _ => None,
            })
        });
        let result = match command {
            DeckCommand::Next => self.deck.next(project.deck.next_keysym(), target),
            DeckCommand::Prev => self.deck.prev(project.deck.prev_keysym(), target),
        };
        if let Err(err) = result {
            tracing::warn!("deck key failed: {}", err.message());
        }
    }

    /// Push a fresh snapshot to the web remote; the server coalesces sends.
    fn publish_remote_state(&self) {
        let remote = self.remote.borrow();
        let Some(remote) = remote.as_ref() else {
            return;
        };
        self.last_publish.set(Instant::now());
        remote.publish(&self.remote_state());
    }

    /// Periodic publish, capped at 2 snapshots per second; actions publish
    /// immediately through [`Self::publish_remote_state`].
    fn publish_remote_state_if_due(&self) {
        if self.last_publish.get().elapsed() < Duration::from_millis(500) {
            return;
        }
        self.publish_remote_state();
    }

    fn remote_state(&self) -> RemoteState {
        let mut state = RemoteState::new(crate::server::Mode::Player, crate::i18n::remote_locale());
        let project = self.controller.project_clone();
        let program = self.controller.program();
        let preview = self.controller.preview_state();
        let queue = self.controller.queue();

        state.project = Some(project.name.clone());
        state.program = crate::server::ProgramInfo {
            name: self.controller.program_name(),
            scene_id: program.scene_id.clone(),
            source: program.source.kind_id().to_string(),
            black: self.controller.is_black(),
            freeze: self.controller.is_frozen(),
            mute: self.controller.is_muted(),
            on_air: !self.controller.is_black() && program.scene_id.is_some(),
            remaining_secs: self.controller.program_remaining(),
        };
        if let Some(playlist) = project.playlist(&preview.playlist_id) {
            state.playlist = crate::server::PlaylistInfo {
                id: playlist.id.clone(),
                name: playlist.name.clone(),
                preview_index: preview.index,
                program_index: program.source.playlist_index(),
                total: playlist.items.len(),
                looping: playlist.r#loop,
                auto: self.controller.auto_advance(),
            };
        }
        state.playlists = self
            .controller
            .playlists()
            .into_iter()
            .map(|(id, name)| crate::server::PlaylistRef { id, name })
            .collect();
        state.queue = crate::server::QueueInfo {
            current: queue.current.as_ref().map(queue_entry_info),
            next: queue.upcoming.iter().map(queue_entry_info).collect(),
            waiting: queue.waiting,
            called: queue.called,
            done: queue.done,
            skipped: queue.skipped,
        };
        state.timers = self
            .controller
            .timer_snapshots()
            .into_iter()
            .map(|snapshot| crate::server::TimerInfo {
                id: snapshot.id,
                name: snapshot.name,
                running: snapshot.running,
                elapsed_secs: snapshot.elapsed_secs,
                remaining_secs: snapshot.remaining_secs,
                end_at: snapshot.end_at.map(timers::format_end_at),
            })
            .collect();
        state.prompter = crate::server::PrompterInfo {
            running: false,
            speed: project.prompter.speed,
        };
        let playing = self.soundboard.borrow();
        state.soundboard = crate::server::SoundboardInfo {
            active_page: project.soundboard.active_page,
            pages: project
                .soundboard
                .pages
                .iter()
                .map(|page| crate::server::SoundboardPageInfo {
                    id: page.id.clone(),
                    name: page.name.clone(),
                    pads: page
                        .pads
                        .iter()
                        .map(|pad| crate::server::PadInfo {
                            id: pad.id.clone(),
                            label: pad.label.clone(),
                            color: pad.color.clone(),
                            hotkey: pad.hotkey.clone().unwrap_or_default(),
                            playing: playing.is_playing(&pad.id),
                        })
                        .collect(),
                })
                .collect(),
        };
        state.deck = self.deck_info(&program);
        state.patterns = PatternKind::ALL
            .iter()
            .map(|kind| crate::server::PatternInfo {
                id: kind.id().to_string(),
                name: rust_i18n::t!(kind.label_key()).to_string(),
            })
            .collect();
        state.pattern_override = program.source.is_pattern();
        state
    }

    fn deck_info(
        &self,
        program: &crate::services::controller::ProgramState,
    ) -> crate::server::DeckInfo {
        let capture = program.scene.as_ref().and_then(|scene| {
            scene.layers.iter().find_map(|layer| match &layer.kind {
                LayerKind::Capture(props) => Some((layer.name.clone(), props.source.clone())),
                _ => None,
            })
        });
        let Some((layer_name, source)) = capture else {
            return crate::server::DeckInfo::default();
        };
        let label = match &source {
            CaptureSource::X11Window { title, .. } if !title.trim().is_empty() => title.clone(),
            CaptureSource::Camera { label, .. } if !label.trim().is_empty() => label.clone(),
            CaptureSource::Portal { label, .. } if !label.trim().is_empty() => label.clone(),
            _ => layer_name,
        };
        crate::server::DeckInfo {
            available: self.deck.available(),
            label,
            backend: self.deck.backend().id().to_string(),
            hint: self.deck.hint().to_string(),
            last_error: self.deck.last_error().unwrap_or_default(),
        }
    }

    fn quit(&self) {
        self.queue.borrow_mut().flush_now();
        if let Some(cookie) = self.inhibit.take() {
            self.app.uninhibit(cookie);
        }
        self.window.close();
        self.app.quit();
    }

    fn shutdown(&self) {
        self.queue.borrow_mut().flush_now();
        self.soundboard.borrow_mut().stop_all();
        if let Some(server) = self.remote.borrow_mut().take() {
            server.stop();
        }
        self.remote_rx.replace(None);
        if let Some(cookie) = self.inhibit.take() {
            self.app.uninhibit(cookie);
        }
        // Player mode never saves; this stops the program/preview media hubs and
        // clears the attached view before the window goes away.
        self.controller.close_project();
        self.window.close();
        tracing::info!("player stopped");
    }
}

fn queue_entry_info(row: &QueueRow) -> crate::server::QueueEntryInfo {
    crate::server::QueueEntryInfo {
        ticket: row.display_ticket(),
        name: row.name.clone(),
        note: row.note.clone(),
    }
}

/// Deck backend for the player: mirror the configured capture backend, and
/// prefer X11 whenever a display is reachable (the portal needs a user prompt).
fn deck_controller(config: &Config) -> DeckController {
    let backend = match config.capture_backend {
        CaptureBackend::X11 => DeckBackend::X11,
        CaptureBackend::Portal => DeckBackend::Portal,
        CaptureBackend::Auto => {
            if x11::display_available() {
                DeckBackend::X11
            } else {
                DeckBackend::Portal
            }
        }
    };
    DeckController::new(backend)
}

/// Start the LAN web remote (blocks until the listener is bound). Returns the
/// handle plus the action receiver drained by the 100 ms tick.
fn start_remote(
    config: &Config,
) -> (
    Option<RemoteServer>,
    Option<tokio::sync::mpsc::Receiver<RemoteAction>>,
) {
    let token = config.remote.token.trim().to_string();
    if token.is_empty() {
        eprintln!(
            "{}",
            rust_i18n::t!("player.remote.no_token", port = config.remote.port)
        );
        return (None, None);
    }
    let (sender, receiver) = tokio::sync::mpsc::channel(64);
    match RemoteServer::start(
        config.remote.port,
        token.clone(),
        crate::server::Mode::Player,
        sender,
    ) {
        Ok(server) => {
            println!(
                "{}",
                rust_i18n::t!(
                    "player.remote.ready",
                    url = server.local_url(),
                    token = token
                )
            );
            (Some(server), Some(receiver))
        }
        Err(err) => {
            eprintln!(
                "{}",
                rust_i18n::t!(
                    "player.remote.failed",
                    port = config.remote.port,
                    error = err.to_string()
                )
            );
            (None, None)
        }
    }
}

/// Per-source queue persistence: `player-<key>.json` under the state directory.
struct QueueSync {
    path: PathBuf,
    rows: Vec<QueueRow>,
    next_ticket: u32,
    pending_since: Option<Instant>,
}

impl QueueSync {
    fn new(path: PathBuf, data: &QueueData) -> Self {
        Self {
            path,
            rows: data.rows.clone(),
            next_ticket: data.next_ticket.max(1),
            pending_since: None,
        }
    }

    fn observe(&mut self, data: &QueueData) {
        if data.next_ticket > self.next_ticket {
            self.next_ticket = data.next_ticket;
        }
        if data.rows != self.rows {
            self.rows = data.rows.clone();
            self.next_ticket = self.next_ticket.max(next_free_ticket(&self.rows));
            self.pending_since = Some(Instant::now());
        }
    }

    fn flush_if_due(&mut self) {
        let Some(since) = self.pending_since else {
            return;
        };
        if since.elapsed() < QUEUE_SAVE_DEBOUNCE {
            return;
        }
        self.flush_now();
    }

    fn flush_now(&mut self) {
        if self.pending_since.take().is_none() {
            return;
        }
        let state = PersistedQueue {
            format: QUEUE_STATE_FORMAT,
            next_ticket: self.next_ticket,
            rows: self.rows.clone(),
        };
        if let Err(err) = queue_state::write_state(&self.path, &state) {
            tracing::warn!("cannot write player queue state: {err}");
        }
    }
}

/// Next ticket number above every numeric ticket currently in the queue.
fn next_free_ticket(rows: &[QueueRow]) -> u32 {
    rows.iter()
        .filter_map(|row| row.ticket.trim().parse::<u32>().ok())
        .max()
        .map(|max| max.saturating_add(1))
        .unwrap_or(1)
}

/// Queue state file contents.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct PersistedQueue {
    format: u32,
    next_ticket: u32,
    rows: Vec<QueueRow>,
}

impl Default for PersistedQueue {
    fn default() -> Self {
        Self {
            format: QUEUE_STATE_FORMAT,
            next_ticket: 1,
            rows: Vec::new(),
        }
    }
}

mod queue_state {
    use super::*;

    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    /// FNV-1a, 64 bit.
    pub(super) fn fnv1a64(bytes: &[u8]) -> u64 {
        let mut hash = FNV_OFFSET;
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash
    }

    fn fnv1a64_add(mut hash: u64, bytes: &[u8]) -> u64 {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash
    }

    /// Stable per-source state key: canonical path, plus size and mtime for a
    /// package so a rebuilt package gets a fresh queue.
    pub(super) fn state_key(canonical: &Path, package_stamp: Option<(u64, u64)>) -> String {
        let mut hash = fnv1a64(canonical.as_os_str().as_encoded_bytes());
        if let Some((size, mtime)) = package_stamp {
            hash = fnv1a64_add(hash, &size.to_le_bytes());
            hash = fnv1a64_add(hash, &mtime.to_le_bytes());
        }
        format!("{hash:016x}")
    }

    pub(super) fn read_state(path: &Path) -> Option<PersistedQueue> {
        let raw = match fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
            Err(err) => {
                tracing::warn!("cannot read player queue state {}: {err}", path.display());
                return None;
            }
        };
        match serde_json::from_str::<PersistedQueue>(&raw) {
            Ok(state) => Some(state),
            Err(err) => {
                tracing::warn!(
                    "corrupt player queue state {}: {err}; using the project rows",
                    path.display()
                );
                None
            }
        }
    }

    pub(super) fn write_state(path: &Path, state: &PersistedQueue) -> anyhow::Result<()> {
        let body = serde_json::to_string_pretty(state)?;
        config::write_private_atomic(path, &body)
    }
}

fn package_stamp(path: &Path) -> Option<(u64, u64)> {
    let metadata = fs::metadata(path).ok()?;
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|time: SystemTime| time.duration_since(UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    Some((metadata.len(), mtime))
}

fn remove_dir(path: &Path) {
    if let Err(err) = fs::remove_dir_all(path) {
        if err.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("cannot remove {}: {err}", path.display());
        }
    }
}

/// Remove extraction directories left behind by an earlier crashed player.
fn cleanup_stale_player_dirs() {
    let root = std::env::temp_dir();
    let Ok(entries) = fs::read_dir(&root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(TEMP_PREFIX) || !entry.path().is_dir() {
            continue;
        }
        let stale = entry
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age > STALE_AFTER);
        if stale {
            remove_dir(&entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_project_exits_with_failure() {
        let dir = std::env::temp_dir().join(format!(
            "signifiage-player-missing-{}",
            uuid::Uuid::new_v4()
        ));
        let args = PlayerArgs {
            path: dir,
            windowed: true,
            monitor: None,
            remote: false,
            port: None,
            token: None,
        };
        assert_eq!(run(&args, Config::default()), glib::ExitCode::FAILURE);
    }

    #[test]
    fn publish_writes_a_loadable_package() {
        let base = std::env::temp_dir().join(format!(
            "signifiage-player-publish-{}",
            uuid::Uuid::new_v4()
        ));
        let project_dir = base.join("demo");
        let package = base.join("dist/demo.signifiage");
        let extracted = base.join("extracted");
        let project = crate::model::Project::from_template(crate::model::Template::Blank, "Demo");
        project::create(&project, &project_dir).unwrap();

        let path = publish(&project_dir, &package).unwrap();
        assert!(path.is_file());
        assert!(path.is_absolute());
        assert_eq!(path, package.canonicalize().unwrap());

        project::extract_package(&path, &extracted).unwrap();
        let loaded = project::load(&extracted).unwrap();
        assert_eq!(loaded.project.name, "Demo");
        assert!(!loaded.project.playlists.is_empty());

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn publish_rejects_a_file() {
        let file = temp_path("publish-file");
        fs::write(&file, b"x").unwrap();
        assert!(publish(&file, &temp_path("out")).is_err());
        fs::remove_file(&file).ok();
    }

    #[test]
    fn fnv_vectors_match_the_reference() {
        assert_eq!(queue_state::fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(queue_state::fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(queue_state::fnv1a64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn state_key_is_stable_and_package_aware() {
        let path = Path::new("/tmp/show.signifiage");
        let first = queue_state::state_key(path, Some((100, 200)));
        assert_eq!(first, queue_state::state_key(path, Some((100, 200))));
        assert_ne!(first, queue_state::state_key(path, Some((101, 200))));
        assert_ne!(first, queue_state::state_key(path, Some((100, 201))));
        assert_ne!(first, queue_state::state_key(path, None));
        assert_eq!(first.len(), 16);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn directory_keys_only_depend_on_the_path() {
        let a = queue_state::state_key(Path::new("/tmp/a"), None);
        let b = queue_state::state_key(Path::new("/tmp/b"), None);
        assert_ne!(a, b);
        assert_eq!(a, queue_state::state_key(Path::new("/tmp/a"), None));
    }

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "signifiage-player-test-{}-{name}.json",
            uuid::Uuid::new_v4()
        ))
    }

    #[test]
    fn queue_state_round_trips() {
        let path = temp_path("queue");
        let mut data = QueueData::default();
        data.next_ticket = 4;
        data.rows = vec![
            QueueRow::new("Ayşe Yılmaz", "001", "İlk çağrı"),
            QueueRow::new("Mehmet Öztürk", "002", ""),
        ];
        let state = PersistedQueue {
            format: QUEUE_STATE_FORMAT,
            next_ticket: data.next_ticket,
            rows: data.rows.clone(),
        };
        queue_state::write_state(&path, &state).unwrap();
        let back = queue_state::read_state(&path).unwrap();
        assert_eq!(back, state);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        fs::remove_file(&path).ok();
    }

    #[test]
    fn corrupt_state_is_ignored() {
        let path = temp_path("corrupt");
        fs::write(&path, b"{not json").unwrap();
        assert!(queue_state::read_state(&path).is_none());
        fs::remove_file(&path).ok();
    }

    #[test]
    fn missing_state_is_not_an_error() {
        assert!(queue_state::read_state(Path::new("/nonexistent/player-x.json")).is_none());
    }

    #[test]
    fn next_ticket_tracks_the_highest_numeric_row() {
        let rows = vec![
            QueueRow::new("a", "007", ""),
            QueueRow::new("b", "A12", ""),
            QueueRow::new("c", "003", ""),
        ];
        assert_eq!(next_free_ticket(&rows), 8);
        assert_eq!(next_free_ticket(&[]), 1);
    }

    #[test]
    fn queue_sync_detects_row_changes() {
        let path = temp_path("sync");
        let mut data = QueueData::default();
        data.next_ticket = 1;
        data.rows = vec![QueueRow::new("first", "001", "")];
        let mut sync = QueueSync::new(path.clone(), &data);
        assert!(sync.pending_since.is_none());
        sync.observe(&data);
        assert!(sync.pending_since.is_none());

        data.rows.push(QueueRow::new("second", "002", ""));
        data.next_ticket = 3;
        let before = sync.next_ticket;
        sync.observe(&data);
        assert!(sync.pending_since.is_some());
        assert!(sync.next_ticket >= before);
        assert_eq!(sync.next_ticket, 3);
        sync.flush_now();
        let back = queue_state::read_state(&path).unwrap();
        assert_eq!(back.rows.len(), 2);
        assert_eq!(back.next_ticket, 3);
        fs::remove_file(&path).ok();
    }
}
