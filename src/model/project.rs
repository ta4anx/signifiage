//! Project document: schema, defaults, validation, persistence, packages.

use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::model::deck::Deck;
use crate::model::layer::{
    CaptureProps, CaptureSource, ClockProps, Layer, LayerKind, PatternProps, QueueProps, Rect,
    Rgba, TextProps, TextStyle, TickerProps,
};
use crate::model::playlist::{Playlist, PlaylistItem};
use crate::model::prompter::Prompter;
use crate::model::queue::QueueData;
use crate::model::schedule::Schedule;
use crate::model::soundboard::Soundboard;
use crate::model::{new_id, now_rfc3339};

pub const FORMAT_VERSION: u32 = 1;
pub const MIN_DESIGN: u32 = 320;
pub const MAX_DESIGN: u32 = 7680;
const MAX_PACKAGE_ENTRIES: usize = 10_000;
const MAX_PACKAGE_ENTRY_BYTES: u64 = 4 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Design {
    pub width: u32,
    pub height: u32,
    pub background: String,
    pub grid: u32,
    pub safe_area: bool,
}

impl Default for Design {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            background: "#0B1220".to_string(),
            grid: 10,
            safe_area: true,
        }
    }
}

impl Design {
    pub fn normalize(&mut self) {
        if self.width == 0 {
            self.width = 1920;
        }
        if self.height == 0 {
            self.height = 1080;
        }
        self.width = self.width.clamp(MIN_DESIGN, MAX_DESIGN);
        self.height = self.height.clamp(MIN_DESIGN, MAX_DESIGN);
        self.grid = self.grid.min(400);
        self.background = Rgba::normalize_hex(&self.background);
    }

    pub fn width_f(&self) -> f64 {
        self.width as f64
    }

    pub fn height_f(&self) -> f64 {
        self.height as f64
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputScale {
    #[default]
    Fit,
    Fill,
    Stretch,
}

impl OutputScale {
    pub fn id(self) -> &'static str {
        match self {
            OutputScale::Fit => "fit",
            OutputScale::Fill => "fill",
            OutputScale::Stretch => "stretch",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "fit" => Some(OutputScale::Fit),
            "fill" => Some(OutputScale::Fill),
            "stretch" => Some(OutputScale::Stretch),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Presentation {
    pub scale: OutputScale,
    pub rotation: u16,
}

impl Presentation {
    pub fn normalize(&mut self) {
        if !matches!(self.rotation, 0 | 90 | 180 | 270) {
            self.rotation = 0;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Scene {
    pub id: String,
    pub name: String,
    pub background: String,
    pub advance_on_media_end: bool,
    pub layers: Vec<Layer>,
}

impl Default for Scene {
    fn default() -> Self {
        Self {
            id: new_id(),
            name: String::new(),
            background: "#0B1220".to_string(),
            advance_on_media_end: false,
            layers: Vec::new(),
        }
    }
}

impl Scene {
    pub fn new(name: impl Into<String>, design: &Design) -> Self {
        Self {
            name: name.into(),
            background: design.background.clone(),
            ..Self::default()
        }
    }

    pub fn layer(&self, id: &str) -> Option<&Layer> {
        self.layers.iter().find(|layer| layer.id == id)
    }

    pub fn layer_mut(&mut self, id: &str) -> Option<&mut Layer> {
        self.layers.iter_mut().find(|layer| layer.id == id)
    }

    pub fn layer_index(&self, id: &str) -> Option<usize> {
        self.layers.iter().position(|layer| layer.id == id)
    }

    /// Topmost layer under a design-space point.
    pub fn hit_layer(&self, x: f64, y: f64) -> Option<&Layer> {
        self.layers.iter().rev().find(|layer| layer.hit(x, y))
    }

    pub fn has_capture(&self) -> bool {
        self.layers
            .iter()
            .any(|layer| matches!(layer.kind, LayerKind::Capture(_)))
    }

    pub fn normalize(&mut self, design: &Design) {
        if self.id.trim().is_empty() {
            self.id = new_id();
        }
        self.background = Rgba::normalize_hex(&self.background);
        for layer in &mut self.layers {
            if layer.id.trim().is_empty() {
                layer.id = new_id();
            }
            layer.normalize(design.width_f(), design.height_f());
        }
    }
}

/// Persisted timer definition; runtime state lives in the timer service.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TimerDef {
    pub id: String,
    pub name: String,
    /// `None` counts up; `Some(secs)` counts down.
    pub duration_secs: Option<u32>,
}

impl Default for TimerDef {
    fn default() -> Self {
        Self {
            id: new_id(),
            name: String::new(),
            duration_secs: None,
        }
    }
}

impl TimerDef {
    pub fn new(id: impl Into<String>, name: impl Into<String>, duration_secs: Option<u32>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            duration_secs,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Project {
    pub format: u32,
    pub name: String,
    pub design: Design,
    pub presentation: Presentation,
    pub scenes: Vec<Scene>,
    pub playlists: Vec<Playlist>,
    pub default_playlist_id: String,
    pub schedules: Vec<Schedule>,
    pub queue: QueueData,
    pub timers: Vec<TimerDef>,
    pub soundboard: Soundboard,
    pub prompter: Prompter,
    pub deck: Deck,
}

impl Default for Project {
    fn default() -> Self {
        Self {
            format: FORMAT_VERSION,
            name: String::new(),
            design: Design::default(),
            presentation: Presentation::default(),
            scenes: Vec::new(),
            playlists: Vec::new(),
            default_playlist_id: String::new(),
            schedules: Vec::new(),
            queue: QueueData::default(),
            timers: vec![
                TimerDef::new("show", "Show", None),
                TimerDef::new("speaker", "Speaker", Some(600)),
            ],
            soundboard: Soundboard::default(),
            prompter: Prompter::default(),
            deck: Deck::default(),
        }
    }
}

impl Project {
    pub fn scene(&self, id: &str) -> Option<&Scene> {
        self.scenes.iter().find(|scene| scene.id == id)
    }

    pub fn scene_mut(&mut self, id: &str) -> Option<&mut Scene> {
        self.scenes.iter_mut().find(|scene| scene.id == id)
    }

    pub fn scene_index(&self, id: &str) -> Option<usize> {
        self.scenes.iter().position(|scene| scene.id == id)
    }

    pub fn playlist(&self, id: &str) -> Option<&Playlist> {
        self.playlists.iter().find(|playlist| playlist.id == id)
    }

    pub fn playlist_mut(&mut self, id: &str) -> Option<&mut Playlist> {
        self.playlists.iter_mut().find(|playlist| playlist.id == id)
    }

    pub fn default_playlist(&self) -> Option<&Playlist> {
        self.playlist(&self.default_playlist_id)
            .or_else(|| self.playlists.first())
    }

    pub fn timer(&self, id: &str) -> Option<&TimerDef> {
        self.timers.iter().find(|timer| timer.id == id)
    }

    /// Scene id at a playlist index, if both exist.
    pub fn scene_at(&self, playlist_id: &str, index: usize) -> Option<&Scene> {
        let playlist = self.playlist(playlist_id)?;
        let scene_id = playlist.scene_id_at(index)?;
        self.scene(scene_id)
    }

    pub fn asset_dir(dir: &Path) -> PathBuf {
        dir.join("assets")
    }

    /// Normalise every field and drop dangling references. Warnings are
    /// user-facing strings already localised.
    pub fn normalize(&mut self, dir: Option<&Path>) -> Vec<String> {
        let mut warnings = Vec::new();
        self.format = FORMAT_VERSION;
        self.design.normalize();
        self.presentation.normalize();

        if self.name.trim().is_empty() {
            self.name = rust_i18n::t!("project.untitled").to_string();
        }

        // Scenes: unique, non-empty ids.
        let mut seen = std::collections::HashSet::new();
        for scene in &mut self.scenes {
            scene.normalize(&self.design);
            while !seen.insert(scene.id.clone()) {
                scene.id = new_id();
            }
        }
        let scene_ids: Vec<String> = self.scenes.iter().map(|scene| scene.id.clone()).collect();

        // Drop invalid asset references.
        let mut bad_assets = 0usize;
        for scene in &mut self.scenes {
            for layer in &mut scene.layers {
                let Some(asset) = layer.kind.asset().map(str::to_string) else {
                    continue;
                };
                if !asset_reference_ok(dir, &asset) {
                    layer.kind.set_asset(None);
                    bad_assets += 1;
                }
            }
        }
        for page in &mut self.soundboard.pages {
            for pad in &mut page.pads {
                let Some(asset) = pad.asset.clone() else {
                    continue;
                };
                if !asset_reference_ok(dir, &asset) {
                    pad.asset = None;
                    bad_assets += 1;
                }
            }
        }
        if bad_assets > 0 {
            warnings
                .push(rust_i18n::t!("project.warning.bad_assets", count = bad_assets).to_string());
        }

        // Playlists: unique ids, valid scene references.
        let mut playlist_ids = std::collections::HashSet::new();
        for playlist in &mut self.playlists {
            if playlist.id.trim().is_empty() {
                playlist.id = new_id();
            }
            while !playlist_ids.insert(playlist.id.clone()) {
                playlist.id = new_id();
            }
            if playlist.name.trim().is_empty() {
                playlist.name = rust_i18n::t!("project.untitled_playlist").to_string();
            }
        }
        let mut dropped_items = 0usize;
        for playlist in &mut self.playlists {
            dropped_items +=
                playlist.retain_existing_scenes(|id| scene_ids.iter().any(|s| s == id));
        }
        if dropped_items > 0 {
            warnings.push(
                rust_i18n::t!(
                    "project.warning.dropped_playlist_items",
                    count = dropped_items
                )
                .to_string(),
            );
        }

        if self.playlists.is_empty() {
            let mut playlist = Playlist::new(rust_i18n::t!("template.playlist.main").to_string());
            if let Some(scene) = self.scenes.first() {
                playlist.items.push(PlaylistItem::new(scene.id.clone(), 0));
            }
            self.playlists.push(playlist);
            warnings.push(rust_i18n::t!("project.warning.no_playlists").to_string());
        }

        if !self
            .playlists
            .iter()
            .any(|p| p.id == self.default_playlist_id)
        {
            if let Some(first) = self.playlists.first() {
                self.default_playlist_id = first.id.clone();
            }
        }

        // Schedules must point at an existing playlist and be valid windows.
        let playlist_ids: Vec<String> = self.playlists.iter().map(|p| p.id.clone()).collect();
        let before = self.schedules.len();
        self.schedules.retain(|schedule| {
            schedule.is_valid() && playlist_ids.iter().any(|id| id == &schedule.playlist_id)
        });
        if before != self.schedules.len() {
            warnings.push(
                rust_i18n::t!(
                    "project.warning.dropped_schedules",
                    count = before - self.schedules.len()
                )
                .to_string(),
            );
        }

        self.queue.normalize();

        // Timer ids must be unique and non-empty.
        let mut timer_ids = std::collections::HashSet::new();
        for timer in &mut self.timers {
            if timer.id.trim().is_empty() {
                timer.id = new_id();
            }
            while !timer_ids.insert(timer.id.clone()) {
                timer.id = new_id();
            }
            if timer.name.trim().is_empty() {
                timer.name = timer.id.clone();
            }
        }

        self.soundboard.normalize();
        self.prompter.normalize();
        self.deck.normalize();
        warnings
    }

    /// New project from a template, already normalised.
    pub fn from_template(template: Template, name: &str) -> Project {
        let mut project = Project::default();
        project.name = name.to_string();

        match template {
            Template::StageShow => {
                let welcome = project.stage_welcome_scene();
                let deck = project.stage_deck_scene();
                project.scenes = vec![welcome, deck];
                let mut show = Playlist::new(rust_i18n::t!("template.playlist.show").to_string());
                show.r#loop = true;
                show.items = project
                    .scenes
                    .iter()
                    .map(|scene| PlaylistItem::new(scene.id.clone(), 0))
                    .collect();
                project.default_playlist_id = show.id.clone();
                project.playlists = vec![show];
                project.prompter.text = rust_i18n::t!("template.prompter.sample").to_string();
                project.timers = vec![
                    TimerDef::new(
                        "show",
                        rust_i18n::t!("template.timers.show").to_string(),
                        None,
                    ),
                    TimerDef::new(
                        "speaker",
                        rust_i18n::t!("template.timers.speaker").to_string(),
                        Some(600),
                    ),
                ];
            }
            Template::SignageLoop => {
                let welcome = project.stage_welcome_scene();
                let announcements = project.signage_announcements_scene();
                project.scenes = vec![welcome, announcements];
                let mut playlist =
                    Playlist::new(rust_i18n::t!("template.playlist.loop").to_string());
                playlist.r#loop = true;
                playlist.items = vec![
                    PlaylistItem::new(project.scenes[0].id.clone(), 10),
                    PlaylistItem::new(project.scenes[1].id.clone(), 15),
                ];
                project.default_playlist_id = playlist.id.clone();
                project.playlists = vec![playlist];
            }
            Template::QueueBoard => {
                let queue = project.queue_scene();
                project.scenes = vec![queue];
                let mut playlist =
                    Playlist::new(rust_i18n::t!("template.playlist.queue").to_string());
                playlist.r#loop = true;
                playlist.items = vec![PlaylistItem::new(project.scenes[0].id.clone(), 0)];
                project.default_playlist_id = playlist.id.clone();
                project.playlists = vec![playlist];
            }
            Template::Blank => {
                let scene = Scene::new(
                    rust_i18n::t!("template.scene.scene1").to_string(),
                    &project.design,
                );
                project.scenes = vec![scene];
                let mut playlist =
                    Playlist::new(rust_i18n::t!("template.playlist.main").to_string());
                playlist.r#loop = true;
                playlist.items = vec![PlaylistItem::new(project.scenes[0].id.clone(), 0)];
                project.default_playlist_id = playlist.id.clone();
                project.playlists = vec![playlist];
            }
        }

        project.normalize(None);
        project
    }

    fn stage_welcome_scene(&self) -> Scene {
        let mut scene = Scene::new(
            rust_i18n::t!("template.scene.welcome").to_string(),
            &self.design,
        );
        let w = self.design.width_f();
        let h = self.design.height_f();
        let mut title = Layer::new(
            LayerKind::Text(TextProps {
                text: rust_i18n::t!("template.stage.welcome_text").to_string(),
                style: TextStyle {
                    size: (h * 0.12).clamp(48.0, 220.0),
                    bold: true,
                    align: crate::model::layer::TextAlign::Center,
                    ..TextStyle::default()
                },
                ..TextProps::default()
            }),
            rust_i18n::t!("layer.type.text").to_string(),
            Rect::new(w * 0.1, h * 0.28, w * 0.8, h * 0.3),
        );
        title.normalize(w, h);
        let mut clock = Layer::new(
            LayerKind::Clock(ClockProps {
                preset: crate::model::layer::ClockPreset::H24,
                show_date: true,
                style: TextStyle {
                    size: (h * 0.07).clamp(28.0, 140.0),
                    align: crate::model::layer::TextAlign::Center,
                    ..TextStyle::default()
                },
            }),
            rust_i18n::t!("layer.type.clock").to_string(),
            Rect::new(w * 0.25, h * 0.62, w * 0.5, h * 0.15),
        );
        clock.normalize(w, h);
        scene.layers = vec![title, clock];
        scene
    }

    fn stage_deck_scene(&self) -> Scene {
        let mut scene = Scene::new(
            rust_i18n::t!("template.scene.deck").to_string(),
            &self.design,
        );
        let layer = Layer::full_bleed(
            LayerKind::Capture(CaptureProps {
                source: CaptureSource::default(),
                ..CaptureProps::default()
            }),
            rust_i18n::t!("template.deck.capture").to_string(),
            self.design.width_f(),
            self.design.height_f(),
        );
        scene.layers = vec![layer];
        scene
    }

    fn signage_announcements_scene(&self) -> Scene {
        let mut scene = Scene::new(
            rust_i18n::t!("template.scene.announcements").to_string(),
            &self.design,
        );
        let w = self.design.width_f();
        let h = self.design.height_f();
        let mut ticker = Layer::new(
            LayerKind::Ticker(TickerProps {
                text: rust_i18n::t!("template.signage.ticker_text").to_string(),
                style: TextStyle {
                    size: (h * 0.06).clamp(24.0, 120.0),
                    ..TextStyle::default()
                },
                ..TickerProps::default()
            }),
            rust_i18n::t!("layer.type.ticker").to_string(),
            Rect::new(0.0, h * 0.78, w, h * 0.16),
        );
        ticker.normalize(w, h);
        let mut clock = Layer::new(
            LayerKind::Clock(ClockProps {
                preset: crate::model::layer::ClockPreset::H24Seconds,
                show_date: true,
                style: TextStyle {
                    size: (h * 0.2).clamp(40.0, 320.0),
                    bold: true,
                    align: crate::model::layer::TextAlign::Center,
                    ..TextStyle::default()
                },
            }),
            rust_i18n::t!("layer.type.clock").to_string(),
            Rect::new(w * 0.1, h * 0.2, w * 0.8, h * 0.4),
        );
        clock.normalize(w, h);
        scene.layers = vec![clock, ticker];
        scene
    }

    /// Full-bleed queue scene used by the queue-board template and imports.
    pub fn queue_scene(&self) -> Scene {
        let mut scene = Scene::new(
            rust_i18n::t!("template.scene.queue").to_string(),
            &self.design,
        );
        let layer = Layer::full_bleed(
            LayerKind::Queue(QueueProps::default()),
            rust_i18n::t!("layer.type.queue").to_string(),
            self.design.width_f(),
            self.design.height_f(),
        );
        scene.layers = vec![layer];
        scene
    }

    /// True when any scene has a queue layer.
    pub fn has_queue_scene(&self) -> bool {
        self.scenes.iter().any(|scene| {
            scene
                .layers
                .iter()
                .any(|layer| matches!(layer.kind, LayerKind::Queue(_)))
        })
    }

    /// Built-in pattern scene (transient, never persisted).
    pub fn pattern_scene(kind: crate::model::layer::PatternKind, design: &Design) -> Scene {
        let mut scene = Scene::new(rust_i18n::t!(kind.label_key()).to_string(), design);
        let layer = Layer::full_bleed(
            LayerKind::Pattern(PatternProps {
                pattern: kind,
                ..PatternProps::default()
            }),
            rust_i18n::t!(kind.label_key()).to_string(),
            design.width_f(),
            design.height_f(),
        );
        scene.layers = vec![layer];
        scene
    }
}

/// Project templates offered by the New Project dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Template {
    StageShow,
    SignageLoop,
    QueueBoard,
    Blank,
}

impl Template {
    pub const ALL: [Template; 4] = [
        Template::StageShow,
        Template::SignageLoop,
        Template::QueueBoard,
        Template::Blank,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Template::StageShow => "stage_show",
            Template::SignageLoop => "signage_loop",
            Template::QueueBoard => "queue_board",
            Template::Blank => "blank",
        }
    }

    pub fn label_key(self) -> &'static str {
        match self {
            Template::StageShow => "template.stage_show.name",
            Template::SignageLoop => "template.signage_loop.name",
            Template::QueueBoard => "template.queue_board.name",
            Template::Blank => "template.blank.name",
        }
    }

    pub fn description_key(self) -> &'static str {
        match self {
            Template::StageShow => "template.stage_show.description",
            Template::SignageLoop => "template.signage_loop.description",
            Template::QueueBoard => "template.queue_board.description",
            Template::Blank => "template.blank.description",
        }
    }
}

/// Lexical check for a stored asset reference.
pub fn asset_reference_ok(dir: Option<&Path>, rel: &str) -> bool {
    let rel = rel.trim();
    if rel.is_empty() {
        return false;
    }
    let path = Path::new(rel);
    if path.is_absolute() {
        return false;
    }
    if !path.components().all(|c| matches!(c, Component::Normal(_))) {
        return false;
    }
    match dir {
        None => true,
        Some(dir) => {
            let assets = dir.join("assets");
            let target = dir.join(path);
            if !target.exists() {
                // Missing files are tolerated; only the lexical form matters.
                return path.starts_with("assets");
            }
            match (assets.canonicalize(), target.canonicalize()) {
                (Ok(canon_assets), Ok(canon_target)) => canon_target.starts_with(canon_assets),
                _ => false,
            }
        }
    }
}

/// A loaded project plus the warnings raised while normalising it.
#[derive(Debug)]
pub struct LoadedProject {
    pub project: Project,
    pub warnings: Vec<String>,
    pub used_backup: bool,
}

fn read_json_file(path: &Path) -> anyhow::Result<Project> {
    let raw =
        fs::read_to_string(path).map_err(|err| anyhow::anyhow!("{}: {err}", path.display()))?;
    serde_json::from_str::<Project>(&raw)
        .map_err(|err| anyhow::anyhow!("{}: {err}", path.display()))
}

/// Load `<dir>/project.json`, falling back to the `.bak` copy once.
pub fn load(dir: &Path) -> anyhow::Result<LoadedProject> {
    let json = dir.join("project.json");
    let bak = dir.join("project.json.bak");
    let mut used_backup = false;
    let mut warnings = Vec::new();

    let mut project = match read_json_file(&json) {
        Ok(project) => project,
        Err(primary) => {
            warn!("cannot read {}: {primary}", json.display());
            match read_json_file(&bak) {
                Ok(project) => {
                    used_backup = true;
                    warnings.push(rust_i18n::t!("project.warning.backup_used").to_string());
                    info!("recovered project from {}", bak.display());
                    project
                }
                Err(secondary) => {
                    return Err(anyhow::anyhow!(
                        "{}\n{}",
                        primary,
                        rust_i18n::t!("project.error.backup_failed", error = secondary.to_string())
                    ));
                }
            }
        }
    };

    warnings.extend(project.normalize(Some(dir)));
    Ok(LoadedProject {
        project,
        warnings,
        used_backup,
    })
}

/// Atomically save `project.json` and keep the previous file as `.bak`.
pub fn save(project: &Project, dir: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(dir)?;
    fs::create_dir_all(Project::asset_dir(dir))?;
    let json = dir.join("project.json");
    let tmp = dir.join("project.json.tmp");
    let bak = dir.join("project.json.bak");

    let body = serde_json::to_string_pretty(project)?;
    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
    }
    if json.exists() {
        fs::copy(&json, &bak)?;
    }
    fs::rename(&tmp, &json)?;
    if let Ok(handle) = fs::File::open(dir) {
        let _ = handle.sync_all();
    }
    Ok(())
}

/// Create a project directory that must be empty or absent.
pub fn create(project: &Project, dir: &Path) -> anyhow::Result<()> {
    if dir.exists() {
        let empty = fs::read_dir(dir)?.next().is_none();
        if !empty {
            anyhow::bail!(
                "{}",
                rust_i18n::t!(
                    "project.error.directory_not_empty",
                    path = dir.display().to_string()
                )
            );
        }
    }
    fs::create_dir_all(dir)?;
    fs::create_dir_all(Project::asset_dir(dir))?;
    save(project, dir)
}

fn zip_time() -> zip::DateTime {
    zip::DateTime::from_date_and_time(2020, 1, 1, 0, 0, 0)
        .unwrap_or_else(|_| zip::DateTime::default())
}

/// Write `project.json` plus every asset into a `.signifiage` package.
pub fn export_package(dir: &Path, output: &Path) -> anyhow::Result<PathBuf> {
    let json = dir.join("project.json");
    if !json.is_file() {
        anyhow::bail!(
            "{}",
            rust_i18n::t!(
                "project.error.missing_json",
                path = json.display().to_string()
            )
        );
    }
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = output.with_extension("signifiage.tmp");
    {
        let file = fs::File::create(&tmp)?;
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .last_modified_time(zip_time())
            .unix_permissions(0o644);
        writer.start_file("project.json", options)?;
        writer.write_all(&fs::read(&json)?)?;

        let assets = Project::asset_dir(dir);
        if assets.is_dir() {
            let mut stack = vec![assets.clone()];
            while let Some(current) = stack.pop() {
                for entry in fs::read_dir(&current)? {
                    let entry = entry?;
                    let path = entry.path();
                    let meta = fs::symlink_metadata(&path)?;
                    if meta.is_dir() {
                        stack.push(path);
                        continue;
                    }
                    if !meta.is_file() {
                        continue;
                    }
                    let rel = path.strip_prefix(dir).map_err(|_| {
                        anyhow::anyhow!("asset outside project: {}", path.display())
                    })?;
                    let name = rel.to_string_lossy().replace('\\', "/");
                    writer.start_file(name, options)?;
                    writer.write_all(&fs::read(&path)?)?;
                }
            }
        }
        writer.finish()?;
    }
    fs::rename(&tmp, output)?;
    Ok(output
        .canonicalize()
        .unwrap_or_else(|_| output.to_path_buf()))
}

/// Reject absolute, parent-traversing and platform-specific entry names.
pub fn sanitize_entry_path(name: &str) -> Option<PathBuf> {
    let normalised = name.replace('\\', "/");
    if normalised.is_empty() || normalised.starts_with('/') {
        return None;
    }
    if normalised.contains(':') && normalised.len() > 1 && normalised.as_bytes()[1] == b':' {
        return None;
    }
    let mut out = PathBuf::new();
    for component in normalised.split('/') {
        match component {
            "" | "." => continue,
            ".." => return None,
            other => out.push(other),
        }
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Because `save` sets no permissions explicitly, keep 0644 for packages.
fn entry_is_symlink(mode: u32) -> bool {
    mode & 0o170000 == 0o120000
}

/// Extract a package into `dest`, refusing unsafe or oversized entries.
pub fn extract_package(archive: &Path, dest: &Path) -> anyhow::Result<()> {
    let file =
        fs::File::open(archive).map_err(|err| anyhow::anyhow!("{}: {err}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|err| anyhow::anyhow!("{}: {err}", archive.display()))?;
    if zip.len() > MAX_PACKAGE_ENTRIES {
        anyhow::bail!(
            "{}",
            rust_i18n::t!("package.error.too_many_entries", count = zip.len())
        );
    }
    fs::create_dir_all(dest)?;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index)?;
        let name = entry.name()?.to_string();
        let Some(relative) = sanitize_entry_path(&name) else {
            anyhow::bail!(
                "{}",
                rust_i18n::t!("package.error.unsafe_entry", name = name.clone())
            );
        };
        if entry.size() > MAX_PACKAGE_ENTRY_BYTES {
            anyhow::bail!(
                "{}",
                rust_i18n::t!("package.error.entry_too_large", name = name.clone())
            );
        }
        if entry_is_symlink(entry.unix_mode().unwrap_or(0)) {
            anyhow::bail!(
                "{}",
                rust_i18n::t!("package.error.symlink_entry", name = name.clone())
            );
        }
        let target = dest.join(&relative);
        if entry.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = fs::File::create(&target)?;
        std::io::copy(&mut entry, &mut out)?;
    }
    if !dest.join("project.json").is_file() {
        anyhow::bail!("{}", rust_i18n::t!("package.error.no_project_json"));
    }
    Ok(())
}

/// Unique asset file name inside `assets/` with `-2`, `-3`, … suffixes.
pub fn unique_asset_name(dir: &Path, file_name: &str) -> String {
    let assets = Project::asset_dir(dir);
    let path = Path::new(file_name);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "asset".to_string());
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
        .unwrap_or_default();
    let mut candidate = format!("{stem}{ext}");
    let mut counter = 2;
    while assets.join(&candidate).exists() {
        candidate = format!("{stem}-{counter}{ext}");
        counter += 1;
    }
    candidate
}

/// Timestamp helper kept here so callers import one module.
pub fn stamp() -> String {
    now_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip_through_json() {
        let project = Project::from_template(Template::StageShow, "Show");
        let json = serde_json::to_string_pretty(&project).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back, project);
    }

    #[test]
    fn minimal_json_gets_defaults() {
        let project: Project = serde_json::from_str("{}").unwrap();
        assert_eq!(project.format, FORMAT_VERSION);
        assert_eq!(project.design.width, 1920);
        assert_eq!(project.timers.len(), 2);
        assert_eq!(project.soundboard.pages.len(), 3);
        assert_eq!(project.queue.next_ticket, 1);
    }

    #[test]
    fn templates_create_expected_structure() {
        let stage = Project::from_template(Template::StageShow, "Stage");
        assert_eq!(stage.scenes.len(), 2);
        assert_eq!(stage.playlists.len(), 1);
        assert_eq!(stage.playlists[0].items.len(), 2);
        assert!(stage.playlists[0].r#loop);
        assert_eq!(stage.playlists[0].id, stage.default_playlist_id);
        assert_eq!(stage.timers.len(), 2);
        assert!(!stage.prompter.text.is_empty());

        let signage = Project::from_template(Template::SignageLoop, "Sign");
        assert_eq!(signage.playlists[0].items[0].duration_secs, 10);
        assert_eq!(signage.playlists[0].items[1].duration_secs, 15);

        let board = Project::from_template(Template::QueueBoard, "Board");
        assert!(board.has_queue_scene());
        assert_eq!(board.playlists[0].items[0].duration_secs, 0);

        let blank = Project::from_template(Template::Blank, "Blank");
        assert_eq!(blank.scenes.len(), 1);
        assert!(blank.scenes[0].layers.is_empty());
        assert_eq!(blank.playlists.len(), 1);
    }

    #[test]
    fn invalid_items_and_schedules_are_dropped_with_warnings() {
        let mut project = Project::from_template(Template::Blank, "P");
        let scene_id = project.scenes[0].id.clone();
        project.playlists[0]
            .items
            .push(PlaylistItem::new("missing-scene", 5));
        project.schedules = vec![
            Schedule {
                playlist_id: project.playlists[0].id.clone(),
                days: vec![0],
                start: "09:00".into(),
                end: "17:00".into(),
            },
            Schedule {
                playlist_id: "nope".into(),
                days: vec![0],
                start: "09:00".into(),
                end: "17:00".into(),
            },
            Schedule {
                playlist_id: project.playlists[0].id.clone(),
                days: vec![0],
                start: "18:00".into(),
                end: "09:00".into(),
            },
        ];
        let warnings = project.normalize(None);
        assert_eq!(project.playlists[0].items.len(), 1);
        assert_eq!(project.playlists[0].items[0].scene_id, scene_id);
        assert_eq!(project.schedules.len(), 1);
        assert!(warnings.iter().any(|w| w.contains("schedule")));
    }

    #[test]
    fn asset_paths_must_stay_relative() {
        assert!(asset_reference_ok(None, "assets/clip.mp4"));
        assert!(!asset_reference_ok(None, "/etc/passwd"));
        assert!(!asset_reference_ok(None, "../secrets.png"));
        assert!(!asset_reference_ok(None, "assets/../../etc/passwd"));
        assert!(!asset_reference_ok(None, ""));
    }

    #[test]
    fn unique_asset_names_get_suffixes() {
        let dir = std::env::temp_dir().join(format!("signifiage-assets-{}", new_id()));
        fs::create_dir_all(Project::asset_dir(&dir)).unwrap();
        assert_eq!(unique_asset_name(&dir, "clip.mp4"), "clip.mp4");
        fs::write(dir.join("assets/clip.mp4"), b"x").unwrap();
        assert_eq!(unique_asset_name(&dir, "clip.mp4"), "clip-2.mp4");
        fs::write(dir.join("assets/clip-2.mp4"), b"x").unwrap();
        assert_eq!(unique_asset_name(&dir, "clip.mp4"), "clip-3.mp4");
        assert_eq!(unique_asset_name(&dir, ".hidden"), ".hidden");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_and_load_round_trip_with_backup() {
        let dir = std::env::temp_dir().join(format!("signifiage-proj-{}", new_id()));
        let project = Project::from_template(Template::StageShow, "Round");
        create(&project, &dir).unwrap();
        let loaded = load(&dir).unwrap();
        assert_eq!(loaded.project, project);
        assert!(!loaded.used_backup);

        // Second save produces a backup of the first.
        let mut changed = project.clone();
        changed.name = "Round 2".into();
        save(&changed, &dir).unwrap();
        assert!(dir.join("project.json.bak").is_file());
        assert_eq!(load(&dir).unwrap().project.name, "Round 2");

        // Corrupt the main file: the backup (old name) is used.
        fs::write(dir.join("project.json"), b"{ not json").unwrap();
        let recovered = load(&dir).unwrap();
        assert!(recovered.used_backup);
        assert_eq!(recovered.project.name, "Round");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_fails_when_both_files_are_broken() {
        let dir = std::env::temp_dir().join(format!("signifiage-bad-{}", new_id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("project.json"), b"nope").unwrap();
        fs::write(dir.join("project.json.bak"), b"also nope").unwrap();
        assert!(load(&dir).is_err());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn create_refuses_non_empty_directory() {
        let dir = std::env::temp_dir().join(format!("signifiage-busy-{}", new_id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("other.txt"), b"x").unwrap();
        let project = Project::from_template(Template::Blank, "B");
        assert!(create(&project, &dir).is_err());
        fs::remove_dir_all(&dir).ok();
    }
}
