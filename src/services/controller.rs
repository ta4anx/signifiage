//! The single owner of live show state: project, program/preview, timers, queue,
//! undo, autosave, outputs and attached scene views.
//!
//! Everything here runs on the GTK main thread; `Rc<Controller>` is shared with the
//! pages and the web bridge.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;

use crate::config::Config;
use crate::model::layer::{LayerKind, PatternKind, Rect};
use crate::model::playlist::Playlist;
use crate::model::project::{self, Project, Scene, Template};
use crate::model::queue::QueueStatus;
use crate::model::{Presentation, new_id};
use crate::services::media::{MediaHub, MediaSource, MediaSpec};
use crate::services::queue::{ImportReport, ImportRow, QueueService, QueueSnapshot};
use crate::services::timers::{
    self, TimerService, format_end_at, format_eta, format_hms, format_remaining,
};
use crate::ui::outputs::OutputManager;
use crate::ui::render::{MediaProvider, QueueEntry, RenderData, SceneView, TimerTexts};

/// Where a view is used; decides which scene and data it receives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ViewRole {
    Editor,
    Preview,
    Program,
}

/// Authoring GUI or standalone player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Gui,
    Player,
}

/// What the program output is showing.
#[derive(Debug, Clone, PartialEq)]
pub enum ProgramSource {
    Black,
    Playlist { playlist_id: String, index: usize },
    Manual { scene_id: String },
    Pattern { kind: PatternKind },
}

impl ProgramSource {
    /// Identifier sent to the web remote.
    pub fn kind_id(&self) -> &'static str {
        match self {
            ProgramSource::Black => "black",
            ProgramSource::Playlist { .. } => "playlist",
            ProgramSource::Manual { .. } => "manual",
            ProgramSource::Pattern { .. } => "pattern",
        }
    }

    pub fn playlist_index(&self) -> Option<usize> {
        match self {
            ProgramSource::Playlist { index, .. } => Some(*index),
            _ => None,
        }
    }

    pub fn is_pattern(&self) -> bool {
        matches!(self, ProgramSource::Pattern { .. })
    }
}

/// Snapshot taken before showing a test pattern.
#[derive(Debug, Clone)]
pub struct SavedProgram {
    pub scene: Option<Scene>,
    pub scene_id: Option<String>,
    pub name: String,
    pub source: ProgramSource,
    pub item_duration: u32,
    pub remaining_secs: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct ProgramState {
    pub scene: Option<Scene>,
    pub scene_id: Option<String>,
    pub name: String,
    pub source: ProgramSource,
    pub item_duration: u32,
    pub saved: Option<Box<SavedProgram>>,
}

impl Default for ProgramState {
    fn default() -> Self {
        Self {
            scene: None,
            scene_id: None,
            name: String::new(),
            source: ProgramSource::Black,
            item_duration: 0,
            saved: None,
        }
    }
}

impl ProgramState {
    pub fn is_black(&self) -> bool {
        self.scene.is_none()
    }
}

#[derive(Debug, Clone, Default)]
pub struct PreviewState {
    pub playlist_id: String,
    pub index: usize,
}

#[derive(Debug, Clone, Default)]
pub struct EditingState {
    pub scene_id: Option<String>,
    pub layer_id: Option<String>,
    pub zoom: f64,
    pub grid: bool,
    pub safe_area: bool,
}

/// Design-owned fields only; queue/timers/soundboard/prompter are never undone.
#[derive(Debug, Clone, PartialEq)]
pub struct DesignSnapshot {
    pub design: crate::model::Design,
    pub presentation: Presentation,
    pub scenes: Vec<Scene>,
    pub playlists: Vec<Playlist>,
    pub schedules: Vec<crate::model::Schedule>,
    pub default_playlist_id: String,
}

#[derive(Debug, Default)]
pub struct UndoManager {
    undo: Vec<(Option<String>, Instant, DesignSnapshot)>,
    redo: Vec<DesignSnapshot>,
    limit: usize,
}

impl UndoManager {
    pub fn new(limit: usize) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            limit,
        }
    }

    /// Push a pre-change snapshot; edits with the same key within 800 ms coalesce.
    pub fn push(&mut self, key: Option<&str>, snapshot: DesignSnapshot) {
        const COALESCE: Duration = Duration::from_millis(800);
        let now = Instant::now();
        if let Some((last_key, at, _)) = self.undo.last() {
            if last_key.as_deref() == key && key.is_some() && now.duration_since(*at) < COALESCE {
                self.redo.clear();
                return;
            }
        }
        self.undo.push((key.map(|k| k.to_string()), now, snapshot));
        if self.undo.len() > self.limit {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self, current: DesignSnapshot) -> Option<DesignSnapshot> {
        let (_, _, snapshot) = self.undo.pop()?;
        self.redo.push(current);
        Some(snapshot)
    }

    pub fn redo(&mut self, current: DesignSnapshot) -> Option<DesignSnapshot> {
        let snapshot = self.redo.pop()?;
        self.undo
            .push((Some("redo".to_string()), Instant::now(), current));
        Some(snapshot)
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    pub fn len(&self) -> usize {
        self.undo.len()
    }

    pub fn is_empty(&self) -> bool {
        self.undo.is_empty()
    }
}

struct ViewLink {
    role: ViewRole,
    view: glib::WeakRef<SceneView>,
}

/// Prompter commands routed from the UI or the web remote.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PrompterCall {
    Start,
    Pause,
    Reset,
    Speed(f64),
}

/// Soundboard commands routed from the UI or the web remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoundboardCall {
    Play(String),
    Stop(String),
    StopAll,
    Page(usize),
}

/// Deck commands routed from the UI or the web remote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeckCall {
    Next,
    Prev,
}

/// State that pages own but the web remote must see.
#[derive(Default)]
pub struct LiveFlags {
    pub prompter_running: Cell<bool>,
    pub prompter_speed: Cell<f64>,
    pub playing_pads: RefCell<Vec<String>>,
    pub soundboard_page: Cell<usize>,
    pub deck_available: Cell<bool>,
    pub deck_backend: RefCell<String>,
    pub deck_hint: RefCell<String>,
    pub deck_error: RefCell<String>,
}

/// Shared-media adapter so views can pull paintables from a hub.
pub struct SharedMedia(pub Rc<RefCell<MediaHub>>);

impl MediaProvider for SharedMedia {
    fn paintable(&self, layer_id: &str) -> Option<gtk::gdk::Paintable> {
        self.0.borrow().paintable(layer_id)
    }
}

pub struct Controller {
    project: RefCell<Project>,
    dir: RefCell<Option<PathBuf>>,
    config: RefCell<Config>,
    program: RefCell<ProgramState>,
    preview: RefCell<PreviewState>,
    editing: RefCell<EditingState>,
    timers: RefCell<TimerService>,
    queue: RefCell<QueueService>,
    undo: RefCell<UndoManager>,
    dirty: Cell<bool>,
    dirty_since: RefCell<Option<Instant>>,
    live_edit: Cell<bool>,
    black: Cell<bool>,
    frozen: Cell<bool>,
    muted: Cell<bool>,
    auto_advance: Cell<bool>,
    advance_at: RefCell<Option<Instant>>,
    advance_remaining: RefCell<Option<Duration>>,
    program_media: Rc<RefCell<MediaHub>>,
    preview_media: Rc<RefCell<MediaHub>>,
    outputs: RefCell<Option<Rc<OutputManager>>>,
    views: RefCell<Vec<ViewLink>>,
    toast: RefCell<Option<Rc<dyn Fn(&str)>>>,
    error: RefCell<Option<Rc<dyn Fn(&str, &str)>>>,
    changed: RefCell<Option<Rc<dyn Fn()>>>,
    last_data_refresh: Cell<Instant>,
    mode: Mode,
    project_open: Cell<bool>,
    app: RefCell<Option<adw::Application>>,
    portal_pending: RefCell<std::collections::HashSet<String>>,
    portal_results: RefCell<
        std::collections::HashMap<
            String,
            std::sync::mpsc::Receiver<
                Result<crate::services::portal::PortalStream, crate::services::portal::PortalError>,
            >,
        >,
    >,
    live: Rc<LiveFlags>,
    prompter_factory: RefCell<Option<Rc<dyn Fn() -> gtk::Widget>>>,
    prompter_hook: RefCell<Option<Rc<dyn Fn(PrompterCall)>>>,
    soundboard_hook: RefCell<Option<Rc<dyn Fn(SoundboardCall)>>>,
    deck_hook: RefCell<Option<Rc<dyn Fn(DeckCall)>>>,
    state_publisher: RefCell<Option<Rc<dyn Fn()>>>,
}

impl Controller {
    pub fn new(config: Config, mode: Mode) -> Rc<Self> {
        let project = Project::default();
        let timers = TimerService::new(&project.timers);
        let queue = QueueService::new(project.queue.clone());
        let program_media = Rc::new(RefCell::new(MediaHub::new()));
        let preview_media = Rc::new(RefCell::new(MediaHub::new()));
        if let Some(device) = &config.audio_sink {
            program_media.borrow_mut().set_sink(Some(device.clone()));
        }
        Rc::new(Self {
            project: RefCell::new(project),
            dir: RefCell::new(None),
            config: RefCell::new(config),
            program: RefCell::new(ProgramState::default()),
            preview: RefCell::new(PreviewState::default()),
            editing: RefCell::new(EditingState {
                zoom: 0.0,
                grid: true,
                safe_area: true,
                ..EditingState::default()
            }),
            timers: RefCell::new(timers),
            queue: RefCell::new(queue),
            undo: RefCell::new(UndoManager::new(50)),
            dirty: Cell::new(false),
            dirty_since: RefCell::new(None),
            live_edit: Cell::new(false),
            black: Cell::new(false),
            frozen: Cell::new(false),
            muted: Cell::new(false),
            auto_advance: Cell::new(true),
            advance_at: RefCell::new(None),
            advance_remaining: RefCell::new(None),
            program_media,
            preview_media,
            outputs: RefCell::new(None),
            views: RefCell::new(Vec::new()),
            toast: RefCell::new(None),
            error: RefCell::new(None),
            changed: RefCell::new(None),
            last_data_refresh: Cell::new(Instant::now()),
            mode,
            project_open: Cell::new(false),
            app: RefCell::new(None),
            portal_pending: RefCell::new(std::collections::HashSet::new()),
            portal_results: RefCell::new(std::collections::HashMap::new()),
            live: Rc::new(LiveFlags::default()),
            prompter_factory: RefCell::new(None),
            prompter_hook: RefCell::new(None),
            soundboard_hook: RefCell::new(None),
            deck_hook: RefCell::new(None),
            state_publisher: RefCell::new(None),
        })
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Runtime flags mirrored by pages (prompter, soundboard, deck).
    pub fn live(&self) -> Rc<LiveFlags> {
        self.live.clone()
    }

    /// Builds the prompter output widget; installed once at startup.
    pub fn set_prompter_factory(&self, factory: Rc<dyn Fn() -> gtk::Widget>) {
        self.prompter_factory.replace(Some(factory));
    }

    pub fn set_prompter_hook(&self, hook: Rc<dyn Fn(PrompterCall)>) {
        self.prompter_hook.replace(Some(hook));
    }

    pub fn set_soundboard_hook(&self, hook: Rc<dyn Fn(SoundboardCall)>) {
        self.soundboard_hook.replace(Some(hook));
    }

    pub fn set_deck_hook(&self, hook: Rc<dyn Fn(DeckCall)>) {
        self.deck_hook.replace(Some(hook));
    }

    pub fn prompter_call(&self, call: PrompterCall) {
        if let Some(hook) = self.prompter_hook.borrow().clone() {
            hook(call);
        }
        self.publish_state();
    }

    pub fn soundboard_call(&self, call: SoundboardCall) {
        if let Some(hook) = self.soundboard_hook.borrow().clone() {
            hook(call);
        }
        self.publish_state();
    }

    pub fn deck_call(&self, call: DeckCall) {
        if let Some(hook) = self.deck_hook.borrow().clone() {
            hook(call);
        }
        self.refresh_views();
        self.notify_changed();
        self.publish_state();
    }

    pub fn set_toast_handler(&self, handler: Rc<dyn Fn(&str)>) {
        self.toast.replace(Some(handler));
    }

    pub fn set_error_handler(&self, handler: Rc<dyn Fn(&str, &str)>) {
        self.error.replace(Some(handler));
    }

    /// Called after any state change so pages can refresh their own chrome.
    pub fn set_changed_handler(&self, handler: Rc<dyn Fn()>) {
        self.changed.replace(Some(handler));
    }

    pub fn toast(&self, message: &str) {
        if let Some(handler) = self.toast.borrow().clone() {
            handler(message);
        } else {
            tracing::info!("toast: {message}");
        }
    }

    pub fn error(&self, title: &str, body: &str) {
        if let Some(handler) = self.error.borrow().clone() {
            handler(title, body);
        } else {
            tracing::error!("{title}: {body}");
        }
    }

    fn notify_changed(&self) {
        if let Some(handler) = self.changed.borrow().clone() {
            handler();
        }
    }

    // ---------------------------------------------------------------- project

    pub fn is_open(&self) -> bool {
        self.project_open.get()
    }

    pub fn project_dir(&self) -> Option<PathBuf> {
        self.dir.borrow().clone()
    }

    pub fn config(&self) -> Config {
        self.config.borrow().clone()
    }

    pub fn set_config(&self, config: Config) {
        *self.config.borrow_mut() = config;
    }

    pub fn with_project<R>(&self, f: impl FnOnce(&Project) -> R) -> R {
        f(&self.project.borrow())
    }

    pub fn project_clone(&self) -> Project {
        self.project.borrow().clone()
    }

    pub fn project_name(&self) -> String {
        self.project.borrow().name.clone()
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty.get()
    }

    /// Snapshot of the design-owned fields for undo.
    fn design_snapshot(&self) -> DesignSnapshot {
        let project = self.project.borrow();
        DesignSnapshot {
            design: project.design.clone(),
            presentation: project.presentation.clone(),
            scenes: project.scenes.clone(),
            playlists: project.playlists.clone(),
            schedules: project.schedules.clone(),
            default_playlist_id: project.default_playlist_id.clone(),
        }
    }

    fn restore_design(&self, snapshot: DesignSnapshot) {
        {
            let mut project = self.project.borrow_mut();
            project.design = snapshot.design;
            project.presentation = snapshot.presentation;
            project.scenes = snapshot.scenes;
            project.playlists = snapshot.playlists;
            project.schedules = snapshot.schedules;
            project.default_playlist_id = snapshot.default_playlist_id;
        }
        self.clamp_editing();
        self.mark_dirty(None);
        self.refresh_views();
        self.notify_changed();
    }

    /// Mutate the project: coalesced undo, dirty marking, live edit and refresh.
    pub fn mutate<R>(&self, key: Option<&str>, f: impl FnOnce(&mut Project) -> R) -> R {
        let before = self.design_snapshot();
        let result = {
            let mut project = self.project.borrow_mut();
            f(&mut project)
        };
        let after = self.design_snapshot();
        if before != after {
            self.undo.borrow_mut().push(key, before);
            self.mark_dirty(key);
            self.sync_services();
            self.apply_live_edit();
            self.refresh_views();
            self.notify_changed();
        }
        result
    }

    /// Mutate without recording undo (runtime-only or load-time changes).
    pub fn mutate_quiet<R>(&self, f: impl FnOnce(&mut Project) -> R) -> R {
        let result = {
            let mut project = self.project.borrow_mut();
            f(&mut project)
        };
        self.mark_dirty(None);
        self.refresh_views();
        result
    }

    /// Editor-only grid/safe-area toggles (not persisted in the project).
    pub fn set_editor_overlays(&self, grid: bool, safe_area: bool) {
        {
            let mut editing = self.editing.borrow_mut();
            editing.grid = grid;
            editing.safe_area = safe_area;
        }
        self.refresh_views();
    }

    /// The derived asset list changed (import/remove/upload).
    pub fn mark_assets_changed(&self) {
        self.refresh_views();
        self.notify_changed();
    }

    fn mark_dirty(&self, key: Option<&str>) {
        let _ = key;
        self.dirty.set(true);
        self.dirty_since.replace(Some(Instant::now()));
    }

    /// Timer definitions and queue source state follow the project.
    fn sync_services(&self) {
        let project = self.project.borrow();
        self.timers.borrow_mut().sync_definitions(&project.timers);
    }

    pub fn undo(&self) -> bool {
        let current = self.design_snapshot();
        let Some(snapshot) = self.undo.borrow_mut().undo(current) else {
            return false;
        };
        self.restore_design(snapshot);
        true
    }

    pub fn redo(&self) -> bool {
        let current = self.design_snapshot();
        let Some(snapshot) = self.undo.borrow_mut().redo(current) else {
            return false;
        };
        self.restore_design(snapshot);
        true
    }

    pub fn can_undo(&self) -> bool {
        self.undo.borrow().can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.undo.borrow().can_redo()
    }

    /// Open a project directory, replacing any open project.
    pub fn open_project(&self, dir: &Path) -> anyhow::Result<()> {
        // `%F` desktop launches may pass the project.json file itself.
        let dir = if dir.is_file() {
            match dir.file_name().and_then(|name| name.to_str()) {
                Some("project.json") => dir.parent().unwrap_or(dir),
                _ => anyhow::bail!(
                    "{}",
                    rust_i18n::t!(
                        "project.error.not_a_project",
                        path = dir.display().to_string()
                    )
                ),
            }
        } else {
            dir
        };
        let loaded = project::load(dir)?;
        self.close_showing_state();
        let mut config = self.config.borrow().clone();
        config.push_recent(dir);
        if let Err(err) = config.save() {
            tracing::warn!("cannot save config: {err}");
        }
        *self.config.borrow_mut() = config.clone();

        {
            let mut project = self.project.borrow_mut();
            *project = loaded.project;
        }
        self.dir.replace(Some(dir.to_path_buf()));
        self.project_open.set(true);
        self.timers
            .borrow_mut()
            .sync_definitions(&self.project.borrow().timers);
        self.queue
            .borrow_mut()
            .replace(self.project.borrow().queue.clone());
        self.undo.borrow_mut().clear();
        self.dirty.set(false);
        self.dirty_since.replace(None);

        let default_playlist = self
            .project
            .borrow()
            .default_playlist()
            .map(|playlist| playlist.id.clone())
            .unwrap_or_default();
        *self.preview.borrow_mut() = PreviewState {
            playlist_id: default_playlist,
            index: 0,
        };
        *self.program.borrow_mut() = ProgramState::default();
        *self.editing.borrow_mut() = EditingState {
            scene_id: self.project.borrow().scenes.first().map(|s| s.id.clone()),
            layer_id: None,
            zoom: 0.0,
            grid: self.project.borrow().design.grid > 0,
            safe_area: self.project.borrow().design.safe_area,
        };
        self.black.set(false);
        self.frozen.set(false);
        self.advance_at.replace(None);

        for warning in loaded.warnings {
            self.toast(&warning);
        }
        self.ensure_outputs();
        self.refresh_views();
        self.notify_changed();
        self.publish_state();
        Ok(())
    }

    /// Create a project from a template in an empty directory.
    pub fn create_project(&self, template: Template, name: &str, dir: &Path) -> anyhow::Result<()> {
        let project = Project::from_template(template, name);
        project::create(&project, dir)?;
        self.open_project(dir)
    }

    /// Stop media/timers/prompter and hide the program before switching projects.
    fn close_showing_state(&self) {
        self.program_media.borrow_mut().stop_all();
        self.portal_pending.borrow_mut().clear();
        self.portal_results.borrow_mut().clear();
        if let Ok(mut cache) = crate::services::portal::cache().lock() {
            cache.clear();
        }
        self.preview_media.borrow_mut().stop_all();
        if let Some(outputs) = self.outputs.borrow().clone() {
            if let Some(view) = outputs.program_view() {
                view.set_scene(None);
            }
            if let Some(view) = outputs.preview_view() {
                view.set_scene(None);
            }
        }
        self.program.replace(ProgramState::default());
        self.preview.replace(PreviewState::default());
        self.advance_at.replace(None);
        self.frozen.set(false);
        self.black.set(false);
    }

    pub fn close_project(&self) {
        if self.is_dirty() && self.project_dir().is_some() {
            if let Err(err) = self.save_now() {
                self.error(
                    &rust_i18n::t!("project.save_status.failed"),
                    &err.to_string(),
                );
                return;
            }
        }
        self.close_showing_state();
        self.project_open.set(false);
        self.dir.replace(None);
        self.project.replace(Project::default());
        self.undo.borrow_mut().clear();
        self.dirty.set(false);
        self.refresh_views();
        self.notify_changed();
    }

    /// Save synchronously; also mirrors queue state into the document.
    pub fn save_now(&self) -> anyhow::Result<()> {
        // The player never writes the project or the package it plays.
        if !self.is_open() || self.mode == Mode::Player {
            return Ok(());
        }
        let Some(dir) = self.project_dir() else {
            return Ok(());
        };
        {
            let queue = self.queue.borrow().data().clone();
            self.project.borrow_mut().queue = queue;
        }
        let result = {
            let project = self.project.borrow();
            project::save(&project, &dir)
        };
        match result {
            Ok(()) => {
                self.dirty.set(false);
                self.dirty_since.replace(None);
                self.notify_changed();
                Ok(())
            }
            Err(err) => {
                self.error(
                    &rust_i18n::t!("project.save_status.failed"),
                    &err.to_string(),
                );
                Err(err)
            }
        }
    }

    /// Autosave: 500 ms after the last mutation.
    pub fn autosave_tick(&self) {
        if !self.dirty.get() || !self.is_open() || self.mode == Mode::Player {
            return;
        }
        let Some(since) = *self.dirty_since.borrow() else {
            return;
        };
        if since.elapsed() < Duration::from_millis(500) {
            return;
        }
        if let Err(err) = self.save_now() {
            tracing::warn!("autosave failed: {err}");
        }
    }

    // ---------------------------------------------------------------- editing

    pub fn editing(&self) -> EditingState {
        self.editing.borrow().clone()
    }

    pub fn editing_scene_id(&self) -> Option<String> {
        self.editing.borrow().scene_id.clone()
    }

    pub fn editing_scene(&self) -> Option<Scene> {
        let id = self.editing.borrow().scene_id.clone()?;
        self.project.borrow().scene(&id).cloned()
    }

    pub fn select_scene(&self, scene_id: &str) {
        let exists = self.project.borrow().scene(scene_id).is_some();
        if !exists {
            return;
        }
        let mut editing = self.editing.borrow_mut();
        editing.scene_id = Some(scene_id.to_string());
        editing.layer_id = None;
        drop(editing);
        self.refresh_views();
        self.notify_changed();
    }

    pub fn insert_scene(&self, name: &str) -> String {
        let design = self.project.borrow().design.clone();
        let scene = Scene::new(name, &design);
        let id = scene.id.clone();
        self.mutate(Some("scene-add"), |project| {
            project.scenes.push(scene);
        });
        self.select_scene(&id);
        id
    }

    pub fn duplicate_scene(&self, scene_id: &str) -> Option<String> {
        let design = self.project.borrow().design.clone();
        let source = self.project.borrow().scene(scene_id).cloned()?;
        let mut copy = source;
        copy.id = new_id();
        copy.name = format!("{} {}", copy.name, rust_i18n::t!("scene.copy_suffix"));
        for layer in &mut copy.layers {
            layer.id = new_id();
        }
        let _ = design;
        let new_id_value = copy.id.clone();
        self.mutate(Some("scene-duplicate"), |project| {
            project.scenes.push(copy);
        });
        self.select_scene(&new_id_value);
        Some(new_id_value)
    }

    pub fn move_scene(&self, scene_id: &str, delta: isize) -> bool {
        let mut moved = false;
        self.mutate(Some("scene-move"), |project| {
            if let Some(index) = project.scene_index(scene_id) {
                let target = index as isize + delta;
                if target >= 0 && (target as usize) < project.scenes.len() {
                    let scene = project.scenes.remove(index);
                    project.scenes.insert(target as usize, scene);
                    moved = true;
                }
            }
        });
        moved
    }

    pub fn delete_scene(&self, scene_id: &str) -> bool {
        let exists = self.project.borrow().scene(scene_id).is_some();
        if !exists {
            return false;
        }
        self.mutate(Some("scene-delete"), |project| {
            project.scenes.retain(|scene| scene.id != scene_id);
            for playlist in &mut project.playlists {
                playlist.remove_scene(scene_id);
            }
        });

        // Program/queue-side consequences.
        {
            let mut program = self.program.borrow_mut();
            if program.scene_id.as_deref() == Some(scene_id) {
                *program = ProgramState::default();
            }
            if let Some(saved) = &program.saved
                && saved.scene_id.as_deref() == Some(scene_id)
            {
                program.saved = None;
            }
        }
        self.clamp_preview();
        {
            let mut editing = self.editing.borrow_mut();
            if editing.scene_id.as_deref() == Some(scene_id) {
                editing.scene_id = self.project.borrow().scenes.first().map(|s| s.id.clone());
                editing.layer_id = None;
            }
        }
        self.refresh_views();
        self.notify_changed();
        true
    }

    pub fn selected_layer(&self) -> Option<String> {
        self.editing.borrow().layer_id.clone()
    }

    pub fn select_layer(&self, layer_id: Option<&str>) {
        self.editing.borrow_mut().layer_id = layer_id.map(|id| id.to_string());
        self.refresh_views();
        self.notify_changed();
    }

    pub fn layer(&self, layer_id: &str) -> Option<crate::model::Layer> {
        let scene_id = self.editing.borrow().scene_id.clone()?;
        let project = self.project.borrow();
        project
            .scene(&scene_id)?
            .layers
            .iter()
            .find(|layer| layer.id == layer_id)
            .cloned()
    }

    pub fn add_layer(&self, kind: LayerKind) {
        let Some(scene_id) = self.editing.borrow().scene_id.clone() else {
            return;
        };
        let design = self.project.borrow().design.clone();
        let name = rust_i18n::t!(kind.label_key()).to_string();
        let rects = default_layer_rect(&kind, &design);
        let mut layer = crate::model::Layer::new(kind, name, rects);
        layer.normalize(design.width_f(), design.height_f());
        let layer_id = layer.id.clone();
        self.mutate(Some("layer-add"), |project| {
            if let Some(scene) = project.scene_mut(&scene_id) {
                scene.layers.push(layer);
            }
        });
        self.select_layer(Some(&layer_id));
    }

    pub fn duplicate_layer(&self, layer_id: &str) -> Option<String> {
        let layer = self.layer(layer_id)?;
        let scene_id = self.editing.borrow().scene_id.clone()?;
        let mut copy = layer;
        copy.id = new_id();
        copy.rect.x += 20.0;
        copy.rect.y += 20.0;
        let new_layer_id = copy.id.clone();
        self.mutate(Some("layer-duplicate"), |project| {
            if let Some(scene) = project.scene_mut(&scene_id) {
                scene.layers.push(copy);
            }
        });
        self.select_layer(Some(&new_layer_id));
        Some(new_layer_id)
    }

    pub fn delete_layer(&self, layer_id: &str) -> bool {
        let Some(scene_id) = self.editing.borrow().scene_id.clone() else {
            return false;
        };
        let mut removed = false;
        self.mutate(Some("layer-delete"), |project| {
            if let Some(scene) = project.scene_mut(&scene_id) {
                let before = scene.layers.len();
                scene.layers.retain(|layer| layer.id != layer_id);
                removed = scene.layers.len() != before;
            }
        });
        if removed {
            self.select_layer(None);
        }
        removed
    }

    pub fn move_layer(&self, layer_id: &str, delta: isize) -> bool {
        let Some(scene_id) = self.editing.borrow().scene_id.clone() else {
            return false;
        };
        let mut moved = false;
        self.mutate(Some("layer-move"), |project| {
            if let Some(scene) = project.scene_mut(&scene_id)
                && let Some(index) = scene.layer_index(layer_id)
            {
                let target = index as isize + delta;
                if target >= 0 && (target as usize) < scene.layers.len() {
                    let layer = scene.layers.remove(index);
                    scene.layers.insert(target as usize, layer);
                    moved = true;
                }
            }
        });
        moved
    }

    /// Edit a layer through a closure; `key` drives undo coalescing.
    pub fn update_layer(
        &self,
        layer_id: &str,
        key: Option<&str>,
        f: impl FnOnce(&mut crate::model::Layer),
    ) {
        let Some(scene_id) = self.editing.borrow().scene_id.clone() else {
            return;
        };
        let design = self.project.borrow().design.clone();
        self.mutate(key, |project| {
            if let Some(scene) = project.scene_mut(&scene_id)
                && let Some(layer) = scene.layer_mut(layer_id)
            {
                f(layer);
                layer.normalize(design.width_f(), design.height_f());
            }
        });
    }

    // ------------------------------------------------------------- playlists

    /// Create a playlist and select it in the preview.
    pub fn add_playlist(&self, name: &str) -> String {
        let playlist = Playlist::new(name);
        let id = playlist.id.clone();
        self.mutate(Some("playlist-add"), |project| {
            project.playlists.push(playlist);
        });
        self.set_preview_playlist(&id);
        id
    }

    pub fn rename_playlist(&self, id: &str, name: &str) {
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        self.mutate(Some("playlist-rename"), |project| {
            if let Some(playlist) = project.playlist_mut(id) {
                playlist.name = name;
            }
        });
    }

    /// Delete a playlist; the last one cannot be removed.
    pub fn delete_playlist(&self, id: &str) -> bool {
        let removable = self.project.borrow().playlists.len() > 1
            && self.project.borrow().playlist(id).is_some();
        if !removable {
            return false;
        }
        let program_on_playlist = matches!(
            &self.program.borrow().source,
            ProgramSource::Playlist { playlist_id, .. } if playlist_id == id
        );
        self.mutate(Some("playlist-delete"), |project| {
            project.playlists.retain(|playlist| playlist.id != id);
            project
                .schedules
                .retain(|schedule| schedule.playlist_id != id);
            if project.default_playlist_id == id {
                project.default_playlist_id = project
                    .playlists
                    .first()
                    .map(|playlist| playlist.id.clone())
                    .unwrap_or_default();
            }
        });
        let fallback = self
            .project
            .borrow()
            .default_playlist()
            .map(|playlist| playlist.id.clone())
            .unwrap_or_default();
        if self.preview.borrow().playlist_id == id {
            *self.preview.borrow_mut() = PreviewState {
                playlist_id: fallback.clone(),
                index: 0,
            };
        }
        if program_on_playlist {
            // Keep the committed scene on air but stop referencing the playlist.
            let mut program = self.program.borrow_mut();
            if let Some(scene_id) = program.scene_id.clone() {
                program.source = ProgramSource::Manual { scene_id };
            } else {
                *program = ProgramState::default();
            }
            drop(program);
            self.advance_at.replace(None);
        }
        self.refresh_views();
        self.notify_changed();
        true
    }

    pub fn set_default_playlist(&self, id: &str) {
        let exists = self.project.borrow().playlist(id).is_some();
        if !exists {
            return;
        }
        self.mutate(Some("playlist-default"), |project| {
            project.default_playlist_id = id.to_string();
        });
    }

    /// Append a scene to the active preview playlist.
    pub fn playlist_add_scene(&self, scene_id: &str) -> bool {
        let playlist_id = self.preview.borrow().playlist_id.clone();
        let exists = self.project.borrow().scene(scene_id).is_some();
        if !exists {
            return false;
        }
        self.mutate(Some("playlist-add-item"), |project| {
            if let Some(playlist) = project.playlist_mut(&playlist_id) {
                playlist
                    .items
                    .push(crate::model::PlaylistItem::new(scene_id, 0));
            }
        });
        true
    }

    pub fn playlist_remove_item(&self, index: usize) -> bool {
        let playlist_id = self.preview.borrow().playlist_id.clone();
        let mut removed = false;
        self.mutate(Some("playlist-remove-item"), |project| {
            if let Some(playlist) = project.playlist_mut(&playlist_id)
                && index < playlist.items.len()
            {
                playlist.items.remove(index);
                removed = true;
            }
        });
        if removed {
            self.clamp_preview();
            self.refresh_views();
        }
        removed
    }

    pub fn playlist_move_item(&self, index: usize, delta: isize) -> bool {
        let playlist_id = self.preview.borrow().playlist_id.clone();
        let mut moved = false;
        self.mutate(Some("playlist-move-item"), |project| {
            if let Some(playlist) = project.playlist_mut(&playlist_id) {
                let target = index as isize + delta;
                if target >= 0 && (target as usize) < playlist.items.len() {
                    moved = playlist.move_item(index, target as usize);
                }
            }
        });
        moved
    }

    /// Import videos and turn each into a full-bleed scene appended to the
    /// current playlist (the "Add videos…" quick signage loop).
    pub fn add_video_scenes(&self, paths: &[std::path::PathBuf]) -> (usize, Vec<String>) {
        let Some(dir) = self.project_dir() else {
            return (0, Vec::new());
        };
        let (imported, skipped) = crate::services::assets::import_files(&dir, paths);
        if imported.is_empty() {
            return (0, skipped);
        }
        let design = self.project.borrow().design.clone();
        let playlist_id = self.preview.borrow().playlist_id.clone();
        let mut created = 0;
        self.mutate(Some("playlist-add-videos"), |project| {
            for asset in &imported {
                let name = std::path::Path::new(asset)
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().to_string())
                    .unwrap_or_else(|| asset.clone());
                let mut scene = Scene::new(name, &design);
                let mut layer = crate::model::Layer::full_bleed(
                    LayerKind::Video(crate::model::layer::VideoProps {
                        asset: Some(asset.clone()),
                        fit: crate::model::layer::Fit::Contain,
                        r#loop: false,
                        volume: 1.0,
                    }),
                    rust_i18n::t!("layer.type.video").to_string(),
                    design.width_f(),
                    design.height_f(),
                );
                layer.normalize(design.width_f(), design.height_f());
                scene.layers.push(layer);
                scene.advance_on_media_end = true;
                let scene_id = scene.id.clone();
                project.scenes.push(scene);
                if let Some(playlist) = project.playlist_mut(&playlist_id) {
                    playlist
                        .items
                        .push(crate::model::PlaylistItem::new(scene_id, 0));
                }
                created += 1;
            }
        });
        self.mark_assets_changed();
        (created, skipped)
    }

    pub fn set_layer_rect(&self, layer_id: &str, rect: Rect, key: Option<&str>) {
        if !rect.is_finite() {
            return;
        }
        self.update_layer(layer_id, key, |layer| layer.rect = rect);
    }

    pub fn toggle_layer_visibility(&self, layer_id: &str) {
        self.update_layer(layer_id, Some("layer-visible"), |layer| {
            layer.visible = !layer.visible;
        });
    }

    fn clamp_editing(&self) {
        let scene_ids: Vec<String> = self
            .project
            .borrow()
            .scenes
            .iter()
            .map(|scene| scene.id.clone())
            .collect();
        let mut editing = self.editing.borrow_mut();
        if let Some(id) = &editing.scene_id
            && !scene_ids.contains(id)
        {
            editing.scene_id = scene_ids.first().cloned();
            editing.layer_id = None;
        }
        if editing.scene_id.is_none() {
            editing.scene_id = scene_ids.first().cloned();
        }
        let valid_layer = editing
            .scene_id
            .as_ref()
            .and_then(|scene_id| self.project.borrow().scene(scene_id).cloned())
            .and_then(|scene| {
                editing
                    .layer_id
                    .as_ref()
                    .map(|layer_id| scene.layer(layer_id).is_some())
            })
            .unwrap_or(false);
        if !valid_layer {
            editing.layer_id = None;
        }
    }

    // --------------------------------------------------------------- preview

    pub fn preview_state(&self) -> PreviewState {
        self.preview.borrow().clone()
    }

    pub fn preview_playlist(&self) -> Option<Playlist> {
        let id = self.preview.borrow().playlist_id.clone();
        self.project
            .borrow()
            .playlist(&id)
            .cloned()
            .or_else(|| self.project.borrow().default_playlist().cloned())
    }

    pub fn preview_scene(&self) -> Option<Scene> {
        let preview = self.preview.borrow().clone();
        let project = self.project.borrow();
        let playlist = project
            .playlist(&preview.playlist_id)
            .or_else(|| project.default_playlist())?;
        let scene_id = playlist.scene_id_at(preview.index)?;
        project.scene(scene_id).cloned()
    }

    pub fn set_preview_index(&self, index: usize) {
        let mut preview = self.preview.borrow_mut();
        let playlist_id = preview.playlist_id.clone();
        let project = self.project.borrow();
        let Some(playlist) = project.playlist(&playlist_id) else {
            return;
        };
        preview.index = index.min(playlist.items.len().saturating_sub(1));
        drop(project);
        drop(preview);
        self.refresh_views();
        self.notify_changed();
        self.publish_state();
    }

    /// Editor selector: changes the preview playlist without taking anything live.
    pub fn set_preview_playlist(&self, playlist_id: &str) {
        let exists = self.project.borrow().playlist(playlist_id).is_some();
        if !exists {
            return;
        }
        let mut preview = self.preview.borrow_mut();
        preview.playlist_id = playlist_id.to_string();
        preview.index = 0;
        drop(preview);
        self.refresh_views();
        self.notify_changed();
        self.publish_state();
    }

    fn clamp_preview(&self) {
        let project = self.project.borrow();
        let mut preview = self.preview.borrow_mut();
        if project.playlist(&preview.playlist_id).is_none() {
            preview.playlist_id = project
                .default_playlist()
                .map(|playlist| playlist.id.clone())
                .unwrap_or_default();
            preview.index = 0;
            return;
        }
        if let Some(playlist) = project.playlist(&preview.playlist_id)
            && preview.index >= playlist.items.len()
        {
            preview.index = playlist.items.len().saturating_sub(1);
        }
    }

    // ------------------------------------------------------------------ show

    pub fn program(&self) -> ProgramState {
        self.program.borrow().clone()
    }

    pub fn program_name(&self) -> String {
        let program = self.program.borrow();
        if program.is_black() {
            rust_i18n::t!("show.black").to_string()
        } else if program.name.is_empty() {
            rust_i18n::t!("show.untitled_scene").to_string()
        } else {
            program.name.clone()
        }
    }

    pub fn is_black(&self) -> bool {
        self.black.get()
    }

    pub fn is_frozen(&self) -> bool {
        self.frozen.get()
    }

    pub fn is_muted(&self) -> bool {
        self.muted.get()
    }

    pub fn auto_advance(&self) -> bool {
        self.auto_advance.get()
    }

    pub fn live_edit(&self) -> bool {
        self.live_edit.get()
    }

    pub fn set_live_edit(&self, on: bool) {
        self.live_edit.set(on);
        if on {
            self.apply_live_edit();
        }
        self.notify_changed();
    }

    /// Remaining time of the committed playlist item, in seconds.
    pub fn program_remaining(&self) -> Option<f64> {
        let program = self.program.borrow();
        if !matches!(program.source, ProgramSource::Playlist { .. }) {
            return None;
        }
        if program.item_duration == 0 {
            return None;
        }
        if self.frozen.get() {
            return self
                .advance_remaining
                .borrow()
                .map(|duration| duration.as_secs_f64());
        }
        self.advance_at
            .borrow()
            .map(|at| at.saturating_duration_since(Instant::now()).as_secs_f64())
    }

    fn restart_advance(&self) {
        let program = self.program.borrow();
        let eligible = matches!(program.source, ProgramSource::Playlist { .. })
            && program.item_duration > 0
            && self.auto_advance.get()
            && !self.frozen.get();
        let duration = Duration::from_secs(u64::from(program.item_duration));
        drop(program);
        if eligible {
            self.advance_at.replace(Some(Instant::now() + duration));
            self.advance_remaining.replace(Some(duration));
        } else {
            self.advance_at.replace(None);
            self.advance_remaining.replace(None);
        }
    }

    fn suspend_advance(&self) {
        if let Some(at) = self.advance_at.borrow_mut().take() {
            let remaining = at.saturating_duration_since(Instant::now());
            self.advance_remaining.replace(Some(remaining));
        }
    }

    fn resume_advance(&self) {
        let remaining = self.advance_remaining.borrow_mut().take();
        if let Some(remaining) = remaining {
            self.advance_at.replace(Some(Instant::now() + remaining));
        }
    }

    /// Commit a scene to program (internal; callers pick the source).
    fn commit_scene(
        &self,
        scene: Scene,
        source: ProgramSource,
        item_duration: u32,
        keep_saved: bool,
    ) {
        {
            let mut program = self.program.borrow_mut();
            program.scene_id = Some(scene.id.clone());
            program.name = scene.name.clone();
            program.scene = Some(scene);
            program.source = source;
            program.item_duration = item_duration;
            if !keep_saved {
                program.saved = None;
            }
        }
        self.black.set(false);
        self.restart_advance();
        self.reconcile_media();
        self.refresh_views();
        self.notify_changed();
        self.publish_state();
    }

    /// Go/Next: take the preview item to program and advance the preview.
    pub fn go(&self) -> bool {
        let preview = self.preview.borrow().clone();
        let project = self.project.borrow();
        let Some(playlist) = project
            .playlist(&preview.playlist_id)
            .or_else(|| project.default_playlist())
        else {
            drop(project);
            self.toast(&rust_i18n::t!("show.no_preview_item"));
            return false;
        };
        let playlist_id = playlist.id.clone();
        let duration = playlist.duration_at(preview.index);
        let Some(scene_id) = playlist.scene_id_at(preview.index).map(|id| id.to_string()) else {
            drop(project);
            self.toast(&rust_i18n::t!("show.no_preview_item"));
            return false;
        };
        let next_index = playlist
            .advance_index(preview.index)
            .unwrap_or(preview.index);
        let Some(scene) = project.scene(&scene_id).cloned() else {
            drop(project);
            self.toast(&rust_i18n::t!("show.no_preview_item"));
            return false;
        };
        drop(project);
        self.commit_scene(
            scene,
            ProgramSource::Playlist {
                playlist_id,
                index: preview.index,
            },
            duration,
            false,
        );
        let active_playlist = self.preview.borrow().playlist_id.clone();
        *self.preview.borrow_mut() = PreviewState {
            playlist_id: active_playlist,
            index: next_index,
        };
        self.refresh_views();
        self.notify_changed();
        self.publish_state();
        true
    }

    /// Previous: back one playlist item from the program (or preview when manual).
    pub fn prev(&self) -> bool {
        let (playlist_id, from_index) = {
            let program = self.program.borrow();
            match &program.source {
                ProgramSource::Playlist { playlist_id, index } => (playlist_id.clone(), *index),
                _ => {
                    let preview = self.preview.borrow().clone();
                    (preview.playlist_id.clone(), preview.index)
                }
            }
        };
        let project = self.project.borrow();
        let Some(playlist) = project
            .playlist(&playlist_id)
            .or_else(|| project.default_playlist())
        else {
            drop(project);
            self.toast(&rust_i18n::t!("show.no_previous_item"));
            return false;
        };
        let playlist_id = playlist.id.clone();
        let Some(target) = playlist.retreat_index(from_index.min(playlist.len().saturating_sub(1)))
        else {
            drop(project);
            self.toast(&rust_i18n::t!("show.no_previous_item"));
            return false;
        };
        let duration = playlist.duration_at(target);
        let Some(scene_id) = playlist.scene_id_at(target).map(|id| id.to_string()) else {
            drop(project);
            self.toast(&rust_i18n::t!("show.no_previous_item"));
            return false;
        };
        let Some(scene) = project.scene(&scene_id).cloned() else {
            drop(project);
            self.toast(&rust_i18n::t!("show.no_previous_item"));
            return false;
        };
        let next_preview = playlist.advance_index(target).unwrap_or(target);
        drop(project);
        self.commit_scene(
            scene,
            ProgramSource::Playlist {
                playlist_id,
                index: target,
            },
            duration,
            false,
        );
        let playlist_id = self.preview.borrow().playlist_id.clone();
        *self.preview.borrow_mut() = PreviewState {
            playlist_id,
            index: next_preview,
        };
        self.refresh_views();
        self.notify_changed();
        self.publish_state();
        true
    }

    /// Editor "Take": clone the scene straight to program, suspend auto-advance.
    pub fn take_scene(&self, scene_id: &str) -> bool {
        let Some(scene) = self.project.borrow().scene(scene_id).cloned() else {
            return false;
        };
        self.commit_scene(
            scene,
            ProgramSource::Manual {
                scene_id: scene_id.to_string(),
            },
            0,
            false,
        );
        self.advance_at.replace(None);
        self.notify_changed();
        true
    }

    pub fn show_pattern(&self, kind: PatternKind) {
        {
            let mut program = self.program.borrow_mut();
            if !program.source.is_pattern() {
                let remaining = self
                    .advance_at
                    .borrow()
                    .map(|at| at.saturating_duration_since(Instant::now()).as_secs_f64());
                program.saved = Some(Box::new(SavedProgram {
                    scene: program.scene.clone(),
                    scene_id: program.scene_id.clone(),
                    name: program.name.clone(),
                    source: program.source.clone(),
                    item_duration: program.item_duration,
                    remaining_secs: remaining,
                }));
            }
        }
        let design = self.project.borrow().design.clone();
        let scene = Project::pattern_scene(kind, &design);
        {
            let mut program = self.program.borrow_mut();
            program.scene_id = Some(scene.id.clone());
            program.name = scene.name.clone();
            program.scene = Some(scene);
            program.source = ProgramSource::Pattern { kind };
            program.item_duration = 0;
        }
        self.suspend_advance();
        self.black.set(false);
        self.program_media.borrow_mut().stop_all();
        self.refresh_views();
        self.notify_changed();
        self.publish_state();
    }

    /// Restore the program that was interrupted by a test pattern.
    pub fn resume_program(&self) {
        let saved = { self.program.borrow_mut().saved.take() };
        let Some(saved) = saved else {
            self.blackout();
            return;
        };
        {
            let mut program = self.program.borrow_mut();
            program.scene = saved.scene;
            program.scene_id = saved.scene_id;
            program.name = saved.name;
            program.source = saved.source;
            program.item_duration = saved.item_duration;
        }
        self.advance_remaining
            .replace(saved.remaining_secs.map(Duration::from_secs_f64));
        self.resume_advance();
        self.reconcile_media();
        self.refresh_views();
        self.notify_changed();
        self.publish_state();
    }

    /// Clear the program to black without touching preview.
    pub fn blackout(&self) {
        {
            let mut program = self.program.borrow_mut();
            *program = ProgramState::default();
        }
        self.advance_at.replace(None);
        self.advance_remaining.replace(None);
        self.program_media.borrow_mut().stop_all();
        self.refresh_views();
        self.notify_changed();
        self.publish_state();
    }

    pub fn set_black(&self, on: bool) {
        self.black.set(on);
        self.refresh_views();
        self.notify_changed();
        self.publish_state();
    }

    pub fn set_auto(&self, on: bool) {
        self.auto_advance.set(on);
        if on {
            self.restart_advance();
        } else {
            self.advance_at.replace(None);
        }
        self.notify_changed();
        self.publish_state();
    }

    /// Freeze captures the program frame and pauses program media + countdown.
    pub fn set_frozen(&self, on: bool) {
        if on == self.frozen.get() {
            return;
        }
        if on {
            let texture = self.program_view().and_then(|view| {
                let width = view.width().max(1);
                let height = view.height().max(1);
                view.render_to_texture(width, height)
            });
            let Some(texture) = texture else {
                tracing::warn!("freeze: cannot capture the program frame");
                self.toast(&rust_i18n::t!("show.freeze_unavailable"));
                return;
            };
            self.suspend_advance();
            self.frozen.set(true);
            self.program_media.borrow_mut().set_paused(true);
            if let Some(view) = self.program_view() {
                view.set_frozen_texture(Some(texture));
            }
        } else {
            self.frozen.set(false);
            if let Some(view) = self.program_view() {
                view.set_frozen_texture(None);
            }
            self.program_media.borrow_mut().set_paused(false);
            self.resume_advance();
        }
        self.refresh_views();
        self.notify_changed();
        self.publish_state();
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.set(muted);
        self.program_media.borrow_mut().set_muted(muted);
        self.notify_changed();
        self.publish_state();
    }

    /// Video EOS on the program output: advance when the plan says so.
    pub fn on_program_eos(&self) {
        let should_advance = {
            let program = self.program.borrow();
            matches!(program.source, ProgramSource::Playlist { .. })
                && program.item_duration == 0
                && program
                    .scene
                    .as_ref()
                    .is_some_and(|scene| scene.advance_on_media_end)
                && self.auto_advance.get()
                && !self.frozen.get()
        };
        if should_advance {
            self.go();
        }
    }

    fn apply_live_edit(&self) {
        if !self.live_edit.get() {
            return;
        }
        let editing = self.editing.borrow().scene_id.clone();
        let Some(editing) = editing else {
            return;
        };
        let mut program = self.program.borrow_mut();
        if program.scene_id.as_deref() != Some(editing.as_str()) || program.source.is_pattern() {
            return;
        }
        let Some(scene) = self.project.borrow().scene(&editing).cloned() else {
            return;
        };
        program.name = scene.name.clone();
        program.scene = Some(scene);
    }

    // ---------------------------------------------------------------- timers

    pub fn timer_snapshots(&self) -> Vec<crate::services::timers::TimerSnapshot> {
        self.timers.borrow().all()
    }

    pub fn timer_start(&self, id: &str) -> bool {
        let changed = self.timers.borrow_mut().start(id);
        if changed {
            self.notify_changed();
            self.publish_state();
        }
        changed
    }

    pub fn timer_pause(&self, id: &str) -> bool {
        let changed = self.timers.borrow_mut().pause(id);
        if changed {
            self.notify_changed();
            self.publish_state();
        }
        changed
    }

    pub fn timer_toggle(&self, id: &str) -> bool {
        let changed = self.timers.borrow_mut().toggle(id);
        if changed {
            self.refresh_views();
            self.notify_changed();
            self.publish_state();
        }
        changed
    }

    pub fn timer_reset(&self, id: &str) -> bool {
        let changed = self.timers.borrow_mut().reset(id);
        if changed {
            self.refresh_views();
            self.notify_changed();
            self.publish_state();
        }
        changed
    }

    pub fn add_timer(&self, name: &str, duration_secs: Option<u32>) -> String {
        let id = new_id();
        let def = crate::model::TimerDef::new(id.clone(), name, duration_secs);
        self.mutate(Some("timer-add"), |project| project.timers.push(def));
        self.timers
            .borrow_mut()
            .sync_definitions(&self.project.borrow().timers);
        id
    }

    pub fn update_timer(&self, id: &str, name: &str, duration_secs: Option<u32>) {
        self.mutate(Some("timer-edit"), |project| {
            if let Some(timer) = project.timers.iter_mut().find(|timer| timer.id == id) {
                timer.name = name.to_string();
                timer.duration_secs = duration_secs;
            }
        });
        self.timers
            .borrow_mut()
            .sync_definitions(&self.project.borrow().timers);
    }

    pub fn remove_timer(&self, id: &str) {
        self.mutate(Some("timer-remove"), |project| {
            project.timers.retain(|timer| timer.id != id);
        });
        self.timers
            .borrow_mut()
            .sync_definitions(&self.project.borrow().timers);
    }

    // ----------------------------------------------------------------- queue

    pub fn queue(&self) -> QueueSnapshot {
        self.queue.borrow().snapshot()
    }

    pub fn queue_can_undo(&self) -> bool {
        self.queue.borrow().can_undo()
    }

    pub fn queue_next(&self) -> bool {
        let changed = self.queue.borrow_mut().next();
        self.after_queue_action(changed)
    }

    pub fn queue_recall(&self) -> bool {
        let changed = self.queue.borrow_mut().recall();
        self.after_queue_action(changed)
    }

    pub fn queue_done(&self) -> bool {
        let changed = self.queue.borrow_mut().done();
        self.after_queue_action(changed)
    }

    pub fn queue_skip(&self) -> bool {
        let changed = self.queue.borrow_mut().skip();
        self.after_queue_action(changed)
    }

    pub fn queue_call(&self, id: &str) -> bool {
        let changed = self.queue.borrow_mut().call(id);
        self.after_queue_action(changed)
    }

    pub fn queue_add(&self, name: &str, note: &str) -> bool {
        let ok = self.queue.borrow_mut().add(name, note).is_ok();
        self.after_queue_action(ok)
    }

    pub fn queue_edit(&self, id: &str, name: &str, note: &str, ticket: &str) -> bool {
        let ok = self.queue.borrow_mut().edit(id, name, note, ticket).is_ok();
        self.after_queue_action(ok)
    }

    pub fn queue_remove(&self, id: &str) -> bool {
        let ok = self.queue.borrow_mut().remove(id).is_ok();
        self.after_queue_action(ok)
    }

    pub fn queue_undo(&self) -> bool {
        let changed = self.queue.borrow_mut().undo();
        self.after_queue_action(changed)
    }

    pub fn queue_merge_import(&self, rows: &[ImportRow]) -> ImportReport {
        let report = self.queue.borrow_mut().merge_import(rows);
        if report.changed {
            self.mark_dirty(None);
            self.refresh_views();
            self.notify_changed();
            self.publish_state();
        }
        report
    }

    pub fn queue_replace(&self, data: crate::model::queue::QueueData) {
        self.queue.borrow_mut().replace(data);
        self.after_queue_action(true);
    }

    fn after_queue_action(&self, changed: bool) -> bool {
        if changed {
            self.mark_dirty(None);
            self.refresh_views();
            self.notify_changed();
            self.publish_state();
        }
        changed
    }

    /// Rows matching a status, for the queue page list.
    pub fn queue_rows(&self) -> Vec<crate::model::QueueRow> {
        self.queue.borrow().data().rows.clone()
    }

    pub fn queue_source(&self) -> Option<crate::model::QueueSource> {
        self.queue.borrow().data().source.clone()
    }

    pub fn set_queue_source(&self, source: Option<crate::model::QueueSource>) {
        {
            let mut project = self.project.borrow_mut();
            project.queue.source = source.clone();
        }
        {
            let mut queue = self.queue.borrow_mut();
            let mut data = queue.data().clone();
            data.source = source;
            queue.replace(data);
        }
        self.mark_dirty(None);
        self.notify_changed();
    }

    // --------------------------------------------------------------- outputs

    /// Remember the application so outputs can be created later.
    pub fn attach_app(&self, app: &adw::Application) {
        self.app.replace(Some(app.clone()));
    }

    /// Create the output manager and apply the configured monitors.
    pub fn ensure_outputs(&self) {
        eprintln!(
            "DBG ensure_outputs: existing={} mode={:?} app={}",
            self.outputs.borrow().is_some(),
            self.mode,
            self.app.borrow().is_some()
        );
        if self.outputs.borrow().is_some() || self.mode == Mode::Player {
            return;
        }
        let Some(app) = self.app.borrow().clone() else {
            return;
        };
        let manager = OutputManager::new(&app);
        if let Some(factory) = self.prompter_factory.borrow().clone() {
            manager.set_prompter_factory(factory);
        }
        let config = self.config.borrow().clone();
        manager.apply(&config);
        if let Some(view) = manager.program_view() {
            view.set_media_provider(Some(Rc::new(SharedMedia(self.program_media.clone()))));
            self.attach_view(ViewRole::Program, &view);
        }
        if let Some(view) = manager.preview_view() {
            view.set_media_provider(Some(Rc::new(SharedMedia(self.preview_media.clone()))));
            self.attach_view(ViewRole::Preview, &view);
        }
        self.outputs.replace(Some(manager));
    }

    pub fn outputs(&self) -> Option<Rc<OutputManager>> {
        self.outputs.borrow().clone()
    }

    /// The live program view: an output window when present, else the player view.
    pub fn program_view(&self) -> Option<SceneView> {
        if let Some(outputs) = self.outputs.borrow().clone()
            && let Some(view) = outputs.program_view()
        {
            return Some(view);
        }
        self.views
            .borrow()
            .iter()
            .filter(|link| link.role == ViewRole::Program)
            .find_map(|link| link.view.upgrade())
    }

    /// Full queue state (rows + live `next_ticket`) for player persistence.
    pub fn queue_data(&self) -> crate::model::queue::QueueData {
        self.queue.borrow().data().clone()
    }

    pub fn apply_output_config(&self) {
        let config = self.config.borrow().clone();
        let Some(outputs) = self.outputs.borrow().clone() else {
            return;
        };
        outputs.apply(&config);
        if let Some(view) = outputs.program_view() {
            view.set_media_provider(Some(Rc::new(SharedMedia(self.program_media.clone()))));
            self.attach_view(ViewRole::Program, &view);
        }
        if let Some(view) = outputs.preview_view() {
            view.set_media_provider(Some(Rc::new(SharedMedia(self.preview_media.clone()))));
            self.attach_view(ViewRole::Preview, &view);
        }
        self.refresh_views();
        // Windows may have opened or closed: let the window reparent special outputs.
        self.notify_changed();
    }

    /// True when a view for this role is currently attached.
    pub fn has_view(&self, role: ViewRole) -> bool {
        self.views
            .borrow()
            .iter()
            .any(|link| link.role == role && link.view.upgrade().is_some())
    }

    pub fn attach_view(&self, role: ViewRole, view: &SceneView) {
        match role {
            ViewRole::Program => {
                view.set_media_provider(Some(Rc::new(SharedMedia(self.program_media.clone()))));
            }
            ViewRole::Preview => {
                view.set_media_provider(Some(Rc::new(SharedMedia(self.preview_media.clone()))));
            }
            ViewRole::Editor => {}
        }
        let mut views = self.views.borrow_mut();
        views.retain(|link| {
            link.view
                .upgrade()
                .is_some_and(|existing| !existing.eq(view))
        });
        views.push(ViewLink {
            role,
            view: view.downgrade(),
        });
        drop(views);
        self.update_view(role, view);
    }

    fn update_view(&self, role: ViewRole, view: &SceneView) {
        let project = self.project.borrow();
        view.set_design(
            project.design.width,
            project.design.height,
            &project.design.background,
            project.presentation.scale,
            project.presentation.rotation,
        );
        drop(project);
        view.set_asset_base(self.project_dir());
        view.set_render_data(self.render_data());

        match role {
            ViewRole::Editor => {
                let editing = self.editing.borrow().clone();
                view.set_scene(self.editing_scene());
                view.set_selection(editing.layer_id.as_deref());
                view.set_zoom(editing.zoom);
                view.set_overlays(editing.grid, editing.safe_area);
                view.set_grid_spacing(f64::from(self.project.borrow().design.grid));
            }
            ViewRole::Preview => {
                let scene = if self.black.get() {
                    None
                } else {
                    self.preview_scene()
                };
                view.set_scene(scene);
                view.refresh_media_links();
            }
            ViewRole::Program => {
                let scene = if self.black.get() {
                    None
                } else {
                    self.program.borrow().scene.clone()
                };
                view.set_scene(scene);
                if self.frozen.get() {
                    // Keep the frozen texture; freeze owns the visual.
                }
                view.refresh_media_links();
            }
        }
    }

    /// Push current state into every attached view.
    pub fn refresh_views(&self) {
        self.clamp_preview();
        let links: Vec<(ViewRole, SceneView)> = self
            .views
            .borrow()
            .iter()
            .filter_map(|link| link.view.upgrade().map(|view| (link.role, view)))
            .collect();
        for (role, view) in links {
            self.update_view(role, &view);
        }
        self.reconcile_media();
    }

    /// Lighter refresh for clock/timer/queue text (4 fps tick).
    pub fn refresh_render_data(&self) {
        let data = self.render_data();
        for link in self.views.borrow().iter() {
            if let Some(view) = link.view.upgrade() {
                view.set_render_data(data.clone());
            }
        }
    }

    /// Assemble the live text data every scene view needs.
    pub fn render_data(&self) -> RenderData {
        let queue = self.queue.borrow().snapshot();
        let current = queue.current.as_ref().map(|row| QueueEntry {
            ticket: row.ticket.clone(),
            name: row.name.clone(),
            note: row.note.clone(),
        });
        let upcoming = queue
            .upcoming
            .iter()
            .map(|row| QueueEntry {
                ticket: row.ticket.clone(),
                name: row.name.clone(),
                note: row.note.clone(),
            })
            .collect();
        let mut timers = std::collections::HashMap::new();
        {
            let service = self.timers.borrow();
            for snapshot in service.all() {
                let remaining = snapshot
                    .remaining_secs
                    .map(format_remaining)
                    .unwrap_or_else(|| timers::MISSING.to_string());
                let end_at = snapshot
                    .end_at
                    .map(format_end_at)
                    .unwrap_or_else(|| timers::MISSING.to_string());
                timers.insert(
                    snapshot.id.clone(),
                    TimerTexts {
                        elapsed: format_hms(snapshot.elapsed_secs),
                        remaining,
                        end_at,
                    },
                );
            }
        }
        let eta_secs = self.program_remaining().map(|secs| secs.ceil() as i64);
        RenderData {
            current,
            upcoming,
            timers,
            eta: Some(format_eta(eta_secs)),
            now: None,
        }
    }

    // ---------------------------------------------------------------- media

    /// Build the media spec list for the program or preview scene.
    fn specs_for(&self, scene: Option<&Scene>, audio: bool) -> Vec<MediaSpec> {
        let Some(scene) = scene else {
            return Vec::new();
        };
        let base = self.project_dir();
        let mut specs = Vec::new();
        for layer in &scene.layers {
            if !layer.visible {
                continue;
            }
            match &layer.kind {
                LayerKind::Video(props) => {
                    let Some(asset) = props.asset.as_deref() else {
                        continue;
                    };
                    let Some(path) = crate::services::media::resolve_asset(base.as_ref(), asset)
                    else {
                        continue;
                    };
                    let Ok(uri) = crate::services::media::file_uri(&path) else {
                        continue;
                    };
                    specs.push(MediaSpec {
                        layer_id: layer.id.clone(),
                        key: format!("video|{uri}|{}|{}", props.r#loop, props.volume),
                        source: MediaSource::Video {
                            uri,
                            repeat: props.r#loop,
                            volume: props.volume,
                        },
                    });
                }
                LayerKind::Capture(props) => {
                    use crate::services::capture::{CaptureError, media_spec_for};
                    let backend = self.config.borrow().capture_backend;
                    match media_spec_for(&layer.id, props, backend, "gtk4paintablesink") {
                        Ok(spec) => specs.push(spec),
                        Err(CaptureError::PortalPending) => {
                            self.request_portal_capture(&layer.id, props);
                        }
                        Err(err) => {
                            tracing::warn!(
                                layer = %layer.id,
                                "capture unavailable: {}",
                                err.message()
                            );
                        }
                    }
                }
                _ => {}
            }
        }
        let _ = audio;
        specs
    }

    /// Ask the desktop portal once per capture layer, then retry on later ticks.
    fn request_portal_capture(&self, layer_id: &str, props: &crate::model::layer::CaptureProps) {
        {
            let mut pending = self.portal_pending.borrow_mut();
            if !pending.insert(layer_id.to_string()) {
                return;
            }
        }
        let restore_token = match &props.source {
            crate::model::layer::CaptureSource::Portal { restore_token, .. } => {
                restore_token.clone()
            }
            _ => None,
        };
        let receiver = crate::services::portal::request_async(layer_id.to_string(), restore_token);
        self.portal_results
            .borrow_mut()
            .insert(layer_id.to_string(), receiver);
        tracing::info!(layer = %layer_id, "requested screen capture permission");
    }

    /// Drain portal results; a granted session makes the next reconcile succeed.
    pub fn poll_portal(&self) {
        let mut finished = Vec::new();
        {
            let mut results = self.portal_results.borrow_mut();
            for (layer_id, receiver) in results.iter() {
                match receiver.try_recv() {
                    Ok(Ok(_stream)) => finished.push((layer_id.clone(), None)),
                    Ok(Err(err)) => finished.push((layer_id.clone(), Some(err.message()))),
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        finished.push((
                            layer_id.clone(),
                            Some(crate::services::portal::PortalError::WorkerStopped.message()),
                        ));
                    }
                }
            }
            for (layer_id, _) in &finished {
                results.remove(layer_id);
                self.portal_pending.borrow_mut().remove(layer_id);
            }
        }
        let mut changed = false;
        for (layer_id, error) in finished {
            match error {
                Some(message) => {
                    tracing::warn!(layer = %layer_id, %message, "screen capture failed");
                    self.toast(&message);
                }
                None => changed = true,
            }
        }
        if changed {
            self.reconcile_media();
            self.refresh_views();
        }
    }

    /// Keep pipelines in sync with the committed program and current preview.
    pub fn reconcile_media(&self) {
        if !self.is_open() {
            return;
        }
        let program_scene = self.program.borrow().scene.clone();
        let black = self.black.get();
        let program_specs = if black {
            Vec::new()
        } else {
            self.specs_for(program_scene.as_ref(), true)
        };
        self.program_media
            .borrow_mut()
            .reconcile(&program_specs, true);

        // Only decode for the preview when something actually shows it.
        if self.has_view(ViewRole::Preview) {
            let preview_specs = self.specs_for(self.preview_scene().as_ref(), false);
            self.preview_media
                .borrow_mut()
                .reconcile(&preview_specs, false);
        } else {
            self.preview_media.borrow_mut().stop_all();
        }

        for (layer_id, message) in self.program_media.borrow_mut().take_errors() {
            tracing::warn!(%layer_id, %message, "program media error");
        }
        for (layer_id, message) in self.preview_media.borrow_mut().take_errors() {
            tracing::warn!(%layer_id, %message, "preview media error");
        }
    }

    /// Drain bus messages and fire EOS semantics.
    pub fn poll_media(&self) {
        let events = self.program_media.borrow_mut().poll();
        for event in events {
            match event {
                crate::services::media::MediaEvent::Eos { .. } => self.on_program_eos(),
                crate::services::media::MediaEvent::Error { message, .. } => {
                    self.toast(&message);
                }
            }
        }
        let _ = self.preview_media.borrow_mut().poll();
    }

    // ------------------------------------------------------------- tick/state

    /// 100 ms heartbeat: autosave, auto-advance, media bus and 4 fps text.
    pub fn tick(&self) {
        self.poll_portal();
        self.poll_media();
        self.autosave_tick();
        self.run_advance();
        self.reconcile_media();
        if self.last_data_refresh.get().elapsed() >= Duration::from_millis(250) {
            self.last_data_refresh.set(Instant::now());
            self.refresh_render_data();
        }
        self.publish_state();
    }

    fn run_advance(&self) {
        let Some(at) = *self.advance_at.borrow() else {
            return;
        };
        if Instant::now() < at {
            return;
        }
        self.advance_at.replace(None);
        self.go();
    }

    /// Hook for the web remote / state publisher (installed by the server).
    pub fn set_state_publisher(&self, publisher: Option<Rc<dyn Fn()>>) {
        self.state_publisher.replace(publisher);
    }

    pub fn publish_state(&self) {
        if let Some(publisher) = self.state_publisher.borrow().clone() {
            publisher();
        }
    }

    /// Commit a playlist item chosen outside the editor (web/player).
    pub fn play_playlist(&self, playlist_id: &str) -> bool {
        let project = self.project.borrow();
        let Some(playlist) = project.playlist(playlist_id) else {
            drop(project);
            return false;
        };
        if playlist.is_empty() {
            drop(project);
            return false;
        }
        let playlist_id = playlist.id.clone();
        let duration = playlist.duration_at(0);
        let Some(scene_id) = playlist.scene_id_at(0).map(|id| id.to_string()) else {
            drop(project);
            return false;
        };
        let Some(scene) = project.scene(&scene_id).cloned() else {
            drop(project);
            return false;
        };
        drop(project);
        self.set_preview_playlist(&playlist_id);
        self.commit_scene(
            scene,
            ProgramSource::Playlist {
                playlist_id,
                index: 0,
            },
            duration,
            false,
        );
        true
    }

    /// Playlist ids/names for selectors.
    pub fn playlists(&self) -> Vec<(String, String)> {
        self.project
            .borrow()
            .playlists
            .iter()
            .map(|playlist| (playlist.id.clone(), playlist.name.clone()))
            .collect()
    }

    /// Queue rows currently waiting, as `(ticket, name)` pairs.
    pub fn queue_upcoming(&self, limit: usize) -> Vec<(String, String)> {
        self.queue
            .borrow()
            .data()
            .upcoming(limit)
            .into_iter()
            .map(|row| (row.ticket.clone(), row.name.clone()))
            .collect()
    }

    /// Counts for the queue page stats.
    pub fn queue_counts(&self) -> [usize; 4] {
        let data = self.queue.borrow();
        let data = data.data();
        [
            data.count(QueueStatus::Waiting),
            data.count(QueueStatus::Called),
            data.count(QueueStatus::Done),
            data.count(QueueStatus::Skipped),
        ]
    }

    pub fn state_publisher(&self) -> Option<Rc<dyn Fn()>> {
        self.state_publisher.borrow().clone()
    }
}

/// Sensible insertion rect for a new layer of a kind.
pub fn default_layer_rect(kind: &LayerKind, design: &crate::model::Design) -> Rect {
    let w = design.width_f();
    let h = design.height_f();
    match kind {
        LayerKind::Queue(_) => Rect::new(0.0, 0.0, w, h),
        LayerKind::Capture(_) | LayerKind::Video(_) | LayerKind::Pattern(_) => {
            Rect::new(w * 0.1, h * 0.1, w * 0.8, h * 0.8)
        }
        LayerKind::Image(_) => Rect::new(w * 0.3, h * 0.3, w * 0.4, h * 0.4),
        LayerKind::Ticker(_) => Rect::new(0.0, h * 0.8, w, h * 0.14),
        LayerKind::Timer(_) | LayerKind::Clock(_) | LayerKind::Eta(_) => {
            Rect::new(w * 0.25, h * 0.15, w * 0.5, h * 0.15)
        }
        LayerKind::Text(_) => Rect::new(w * 0.125, h * 0.4, w * 0.75, h * 0.2),
    }
}
