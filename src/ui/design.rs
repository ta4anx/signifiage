//! Design page: scenes, playlist, assets, the canvas and the inspector.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::gdk;
use gtk::gio;
use gtk::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;
use rust_i18n::t;

use crate::model::layer::{
    CaptureProps, ClockProps, EtaProps, ImageProps, LayerKind, PatternProps, QueueProps, Rect,
    TickerProps, TimerProps, VideoProps,
};
use crate::services::assets;
use crate::services::controller::{Controller, ProgramSource};
use crate::ui::{Page, canvas::Canvas, dialogs, inspector::Inspector, widgets};

pub struct DesignPage {
    pub root: gtk::Box,
    controller: Rc<Controller>,
    overlay: adw::ToastOverlay,
    canvas: Rc<Canvas>,
    inspector: Rc<Inspector>,
    scenes_list: gtk::ListBox,
    scenes_signature: RefCell<String>,
    playlist_dropdown: gtk::DropDown,
    playlist_items: RefCell<Vec<(String, String)>>,
    playlist_list: gtk::ListBox,
    playlist_signature: RefCell<String>,
    duration_spin: gtk::SpinButton,
    assets_list: gtk::ListBox,
    assets_signature: RefCell<String>,
    selected_asset: RefCell<Option<String>>,
    zoom_dropdown: gtk::DropDown,
    grid_toggle: gtk::ToggleButton,
    safe_toggle: gtk::ToggleButton,
    updating: RefCell<bool>,
}

impl DesignPage {
    pub fn new(controller: Rc<Controller>, overlay: adw::ToastOverlay) -> Rc<Self> {
        let canvas = Canvas::new(controller.clone());
        let inspector = Rc::new(Inspector::new(controller.clone(), overlay.clone()));

        let scenes_list = gtk::ListBox::new();
        scenes_list.set_selection_mode(gtk::SelectionMode::Single);
        scenes_list.add_css_class("boxed-list");

        let playlist_dropdown = gtk::DropDown::from_strings(&[]);
        let playlist_list = gtk::ListBox::new();
        playlist_list.set_selection_mode(gtk::SelectionMode::Single);
        playlist_list.add_css_class("boxed-list");
        let duration_spin = widgets::int_spin(0, 86_400, 0);

        let assets_list = gtk::ListBox::new();
        assets_list.set_selection_mode(gtk::SelectionMode::Single);
        assets_list.add_css_class("boxed-list");

        let zoom_dropdown = widgets::dropdown(
            &[
                t!("zoom.fit").to_string(),
                "50%".to_string(),
                "75%".to_string(),
                "100%".to_string(),
            ],
            0,
        );
        let grid_toggle = gtk::ToggleButton::with_label(&t!("design.grid").to_string());
        grid_toggle.set_active(true);
        let safe_toggle = gtk::ToggleButton::with_label(&t!("design.safe_area").to_string());
        safe_toggle.set_active(true);

        let design = Rc::new(Self {
            root: gtk::Box::new(gtk::Orientation::Vertical, 0),
            controller: controller.clone(),
            overlay: overlay.clone(),
            canvas: canvas.clone(),
            inspector: inspector.clone(),
            scenes_list: scenes_list.clone(),
            scenes_signature: RefCell::new(String::new()),
            playlist_dropdown: playlist_dropdown.clone(),
            playlist_items: RefCell::new(Vec::new()),
            playlist_list: playlist_list.clone(),
            playlist_signature: RefCell::new(String::new()),
            duration_spin: duration_spin.clone(),
            assets_list: assets_list.clone(),
            assets_signature: RefCell::new(String::new()),
            selected_asset: RefCell::new(None),
            zoom_dropdown: zoom_dropdown.clone(),
            grid_toggle: grid_toggle.clone(),
            safe_toggle: safe_toggle.clone(),
            updating: RefCell::new(false),
        });

        design.build_toolbar();
        design.build_layout();
        design.wire_signals();
        design.refresh();
        design
    }

    fn build_layout(&self) {
        // Sidebar | (canvas | inspector), under the toolbar row.
        let centre = gtk::Paned::new(gtk::Orientation::Horizontal);
        self.canvas.container.set_size_request(320, 240);
        centre.set_start_child(Some(&self.canvas.container));
        centre.set_end_child(Some(&self.inspector.panel));
        centre.set_position(760);
        centre.set_resize_start_child(true);
        centre.set_shrink_start_child(false);
        centre.set_wide_handle(true);

        let outer = gtk::Paned::new(gtk::Orientation::Horizontal);
        outer.set_start_child(Some(&self.sidebar()));
        outer.set_end_child(Some(&centre));
        outer.set_position(320);
        outer.set_resize_start_child(false);
        outer.set_shrink_start_child(false);
        outer.set_vexpand(true);
        self.root.append(&outer);
    }

    fn sidebar(&self) -> gtk::Box {
        let side = gtk::Box::new(gtk::Orientation::Vertical, 12);
        side.set_margin_top(12);
        side.set_margin_bottom(12);
        side.set_margin_start(12);
        side.set_margin_end(12);
        side.set_width_request(300);

        // Scenes.
        let (scenes_section, scenes_content) = widgets::section(&t!("design.scenes").to_string());
        let scene_buttons = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let add = widgets::icon_button("list-add-symbolic", &t!("scene.add").to_string());
        let duplicate =
            widgets::icon_button("edit-copy-symbolic", &t!("scene.duplicate").to_string());
        let delete = widgets::icon_button("user-trash-symbolic", &t!("scene.delete").to_string());
        let up = widgets::icon_button("go-up-symbolic", &t!("scene.move_up").to_string());
        let down = widgets::icon_button("go-down-symbolic", &t!("scene.move_down").to_string());
        scene_buttons.append(&add);
        scene_buttons.append(&duplicate);
        scene_buttons.append(&delete);
        scene_buttons.append(&up);
        scene_buttons.append(&down);
        scenes_content.append(&scene_buttons);
        let scenes_scroll = gtk::ScrolledWindow::builder()
            .child(&self.scenes_list)
            .min_content_height(140)
            .build();
        scenes_content.append(&scenes_scroll);
        side.append(&scenes_section);

        {
            let controller = self.controller.clone();
            add.connect_clicked(move |_| {
                let index = controller.project_clone().scenes.len() + 1;
                controller.insert_scene(&format!("{} {}", t!("scene.new_name"), index));
            });
        }
        {
            let controller = self.controller.clone();
            duplicate.connect_clicked(move |_| {
                if let Some(id) = controller.editing_scene_id() {
                    controller.duplicate_scene(&id);
                }
            });
        }
        {
            let controller = self.controller.clone();
            let overlay = self.overlay.clone();
            delete.connect_clicked(move |_| {
                let Some(id) = controller.editing_scene_id() else {
                    return;
                };
                let name = controller
                    .project_clone()
                    .scene(&id)
                    .map(|scene| scene.name.clone())
                    .unwrap_or_default();
                let controller_confirm = controller.clone();
                let overlay_confirm = overlay.clone();
                widgets::confirm(
                    None,
                    &t!("scene.delete").to_string(),
                    &t!("scene.delete_confirm", name = name).to_string(),
                    move || {
                        if controller_confirm.delete_scene(&id) {
                            widgets::toast(&overlay_confirm, &t!("scene.deleted").to_string());
                        }
                    },
                );
            });
        }
        {
            let controller = self.controller.clone();
            up.connect_clicked(move |_| {
                if let Some(id) = controller.editing_scene_id() {
                    controller.move_scene(&id, -1);
                }
            });
        }
        {
            let controller = self.controller.clone();
            down.connect_clicked(move |_| {
                if let Some(id) = controller.editing_scene_id() {
                    controller.move_scene(&id, 1);
                }
            });
        }

        // Playlist.
        let (playlist_section, playlist_content) =
            widgets::section(&t!("design.playlist").to_string());
        let playlist_buttons = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let new_playlist =
            widgets::icon_button("list-add-symbolic", &t!("playlist.new").to_string());
        let rename_playlist =
            widgets::icon_button("document-edit-symbolic", &t!("playlist.rename").to_string());
        let delete_playlist =
            widgets::icon_button("user-trash-symbolic", &t!("playlist.delete").to_string());
        let default_playlist =
            widgets::icon_button("starred-symbolic", &t!("playlist.make_default").to_string());
        playlist_buttons.append(&new_playlist);
        playlist_buttons.append(&rename_playlist);
        playlist_buttons.append(&delete_playlist);
        playlist_buttons.append(&default_playlist);
        playlist_content.append(&widgets::labelled(
            &t!("playlist.active").to_string(),
            &self.playlist_dropdown,
        ));
        playlist_content.append(&playlist_buttons);

        let item_buttons = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let add_scene = widgets::icon_button(
            "insert-object-symbolic",
            &t!("playlist.add_scene").to_string(),
        );
        let add_videos = widgets::button(&t!("playlist.add_videos").to_string());
        let remove_item = widgets::icon_button(
            "list-remove-symbolic",
            &t!("playlist.remove_item").to_string(),
        );
        let item_up = widgets::icon_button("go-up-symbolic", &t!("playlist.move_up").to_string());
        let item_down =
            widgets::icon_button("go-down-symbolic", &t!("playlist.move_down").to_string());
        item_buttons.append(&add_scene);
        item_buttons.append(&add_videos);
        item_buttons.append(&remove_item);
        item_buttons.append(&item_up);
        item_buttons.append(&item_down);
        playlist_content.append(&item_buttons);
        playlist_content.append(&widgets::labelled(
            &t!("playlist.duration").to_string(),
            &self.duration_spin,
        ));
        let playlist_scroll = gtk::ScrolledWindow::builder()
            .child(&self.playlist_list)
            .min_content_height(120)
            .build();
        playlist_content.append(&playlist_scroll);
        let schedules = widgets::button(&t!("schedule.edit").to_string());
        playlist_content.append(&schedules);
        side.append(&playlist_section);

        {
            let controller = self.controller.clone();
            new_playlist.connect_clicked(move |_| {
                let controller = controller.clone();
                dialogs::prompt_text(
                    None,
                    &t!("playlist.new").to_string(),
                    &t!("playlist.new_name").to_string(),
                    move |name| {
                        if name.trim().is_empty() {
                            return;
                        }
                        controller.add_playlist(name.trim());
                    },
                );
            });
        }
        {
            let controller = self.controller.clone();
            rename_playlist.connect_clicked(move |_| {
                let id = controller.preview_state().playlist_id;
                let current = controller
                    .project_clone()
                    .playlist(&id)
                    .map(|playlist| playlist.name.clone())
                    .unwrap_or_default();
                let controller_confirm = controller.clone();
                let id_for_prompt = id.clone();
                dialogs::prompt_text(
                    None,
                    &t!("playlist.rename").to_string(),
                    &current,
                    move |name| controller_confirm.rename_playlist(&id_for_prompt, &name),
                );
            });
        }
        {
            let controller = self.controller.clone();
            let overlay = self.overlay.clone();
            delete_playlist.connect_clicked(move |_| {
                let id = controller.preview_state().playlist_id;
                if !controller.delete_playlist(&id) {
                    widgets::toast(&overlay, &t!("playlist.last_blocked").to_string());
                }
            });
        }
        {
            let controller = self.controller.clone();
            let overlay = self.overlay.clone();
            default_playlist.connect_clicked(move |_| {
                let id = controller.preview_state().playlist_id;
                controller.set_default_playlist(&id);
                widgets::toast(&overlay, &t!("playlist.default_set").to_string());
            });
        }
        {
            let controller = self.controller.clone();
            let overlay = self.overlay.clone();
            add_scene.connect_clicked(move |_| {
                let Some(scene_id) = controller.editing_scene_id() else {
                    return;
                };
                if controller.playlist_add_scene(&scene_id) {
                    widgets::toast(&overlay, &t!("playlist.item_added").to_string());
                }
            });
        }
        {
            let controller = self.controller.clone();
            let overlay = self.overlay.clone();
            add_videos.connect_clicked(move |_| {
                pick_files(
                    &overlay,
                    &t!("playlist.add_videos").to_string(),
                    &["mp4", "webm", "mkv", "mov", "avi", "m4v", "ts", "ogv"],
                    false,
                    {
                        let controller = controller.clone();
                        let overlay = overlay.clone();
                        move |paths| {
                            let (created, skipped) = controller.add_video_scenes(&paths);
                            if created > 0 {
                                widgets::toast(
                                    &overlay,
                                    &t!("playlist.videos_added", count = created).to_string(),
                                );
                            }
                            if !skipped.is_empty() {
                                widgets::toast(
                                    &overlay,
                                    &t!("assets.skipped", count = skipped.len()).to_string(),
                                );
                            }
                        }
                    },
                );
            });
        }
        {
            let controller = self.controller.clone();
            remove_item.connect_clicked(move |_| {
                let index = controller.preview_state().index;
                controller.playlist_remove_item(index);
            });
        }
        {
            let controller = self.controller.clone();
            item_up.connect_clicked(move |_| {
                let index = controller.preview_state().index;
                controller.playlist_move_item(index, -1);
            });
        }
        {
            let controller = self.controller.clone();
            item_down.connect_clicked(move |_| {
                let index = controller.preview_state().index;
                controller.playlist_move_item(index, 1);
            });
        }
        {
            let controller = self.controller.clone();
            let overlay = self.overlay.clone();
            schedules.connect_clicked(move |_| {
                let parent = overlay
                    .root()
                    .and_then(|root| root.downcast::<gtk::Window>().ok());
                dialogs::schedules_dialog(&controller, &overlay, parent.as_ref());
            });
        }

        // Assets.
        let (assets_section, assets_content) = widgets::section(&t!("design.assets").to_string());
        let asset_buttons = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let import = widgets::button(&t!("assets.import").to_string());
        let reveal = widgets::icon_button("folder-open-symbolic", &t!("assets.reveal").to_string());
        let remove_asset =
            widgets::icon_button("user-trash-symbolic", &t!("assets.remove").to_string());
        asset_buttons.append(&import);
        asset_buttons.append(&reveal);
        asset_buttons.append(&remove_asset);
        assets_content.append(&asset_buttons);
        let assets_scroll = gtk::ScrolledWindow::builder()
            .child(&self.assets_list)
            .min_content_height(140)
            .vexpand(true)
            .build();
        assets_content.append(&assets_scroll);
        side.append(&assets_section);

        {
            let controller = self.controller.clone();
            let overlay = self.overlay.clone();
            import.connect_clicked(move |_| {
                pick_files(
                    &overlay,
                    &t!("assets.import").to_string(),
                    &[
                        "png", "jpg", "jpeg", "webp", "gif", "bmp", "svg", "mp4", "webm", "mkv",
                        "mov", "wav", "mp3", "ogg", "flac", "m4a",
                    ],
                    true,
                    {
                        let controller = controller.clone();
                        let overlay = overlay.clone();
                        move |paths| {
                            import_paths(&controller, &overlay, &paths);
                        }
                    },
                );
            });
        }
        {
            let controller = self.controller.clone();
            let overlay = self.overlay.clone();
            reveal.connect_clicked(move |_| {
                let Some(dir) = controller.project_dir() else {
                    return;
                };
                let uri = format!("file://{}", dir.join("assets").display());
                if let Err(err) =
                    gio::AppInfo::launch_default_for_uri(&uri, gio::AppLaunchContext::NONE)
                {
                    widgets::toast(&overlay, &err.to_string());
                }
            });
        }
        {
            let controller = self.controller.clone();
            let overlay = self.overlay.clone();
            let assets_list = self.assets_list.clone();
            let selected = self.selected_asset.clone();
            remove_asset.connect_clicked(move |_| {
                let Some(asset) = selected.borrow().clone() else {
                    widgets::toast(&overlay, &t!("assets.select_first").to_string());
                    return;
                };
                let Some(dir) = controller.project_dir() else {
                    return;
                };
                let project = controller.project_clone();
                let controller_confirm = controller.clone();
                let overlay_confirm = overlay.clone();
                let assets_list = assets_list.clone();
                widgets::confirm(
                    None,
                    &t!("assets.remove").to_string(),
                    &t!("assets.remove_confirm", name = asset.clone()).to_string(),
                    move || match assets::remove(&dir, &asset, &project) {
                        Ok(()) => {
                            controller_confirm.mark_assets_changed();
                            let _ = &assets_list;
                            widgets::toast(&overlay_confirm, &t!("assets.removed").to_string());
                        }
                        Err(_) => {
                            widgets::toast(&overlay_confirm, &t!("assets.in_use").to_string())
                        }
                    },
                );
            });
        }

        side
    }

    fn build_toolbar(&self) {
        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        toolbar.set_margin_top(8);
        toolbar.set_margin_bottom(8);
        toolbar.set_margin_start(12);
        toolbar.set_margin_end(12);

        let add_menu = gtk::MenuButton::new();
        add_menu.set_label(&t!("design.add_layer").to_string());
        let popover = gtk::Popover::new();
        let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
        for (key, kind) in layer_kinds() {
            let item = widgets::button(&t!(key).to_string());
            let controller = self.controller.clone();
            item.connect_clicked(move |_| controller.add_layer(kind.clone()));
            list.append(&item);
        }
        popover.set_child(Some(&list));
        add_menu.set_popover(Some(&popover));
        toolbar.append(&add_menu);

        toolbar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        toolbar.append(&widgets::labelled(
            &t!("design.zoom").to_string(),
            &self.zoom_dropdown,
        ));
        toolbar.append(&self.grid_toggle);
        toolbar.append(&self.safe_toggle);

        let take = widgets::suggested(&t!("design.take").to_string());
        {
            let controller = self.controller.clone();
            let overlay = self.overlay.clone();
            take.connect_clicked(move |_| {
                let Some(scene_id) = controller.editing_scene_id() else {
                    return;
                };
                if controller.take_scene(&scene_id) {
                    widgets::toast(
                        &overlay,
                        &t!("design.taken", name = controller.program_name()).to_string(),
                    );
                }
            });
        }
        toolbar.append(&take);

        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        toolbar.append(&spacer);

        // File drop on the canvas imports and creates a media layer.
        let drop_target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        {
            let controller = self.controller.clone();
            let overlay = self.overlay.clone();
            let canvas = self.canvas.clone();
            drop_target.connect_drop(move |_, value, x, y| {
                let Ok(files) = value.get::<gdk::FileList>() else {
                    return false;
                };
                let paths: Vec<PathBuf> = files.files().iter().filter_map(|f| f.path()).collect();
                if paths.is_empty() {
                    return false;
                }
                let imported = import_paths(&controller, &overlay, &paths);
                if let Some(first) = imported.first() {
                    let (dx, dy) = canvas.view.view_to_design(x, y);
                    create_media_layer(&controller, first, dx, dy);
                }
                true
            });
        }
        self.canvas.view.add_controller(drop_target);

        self.root.append(&toolbar);
    }

    fn wire_signals(&self) {
        {
            let controller = self.controller.clone();
            let updating = self.updating.clone();
            self.scenes_list.connect_row_selected(move |_, row| {
                let Some(row) = row else {
                    return;
                };
                if *updating.borrow() {
                    return;
                }
                let id = row.widget_name().to_string();
                if !id.is_empty() {
                    controller.select_scene(&id);
                }
            });
        }

        {
            let canvas = self.canvas.clone();
            self.zoom_dropdown.connect_selected_notify(move |dropdown| {
                let zoom = match dropdown.selected() {
                    1 => 0.5,
                    2 => 0.75,
                    3 => 1.0,
                    _ => 0.0,
                };
                canvas.set_zoom(zoom);
            });
        }

        {
            let controller = self.controller.clone();
            let safe = self.safe_toggle.clone();
            self.grid_toggle.connect_toggled(move |button| {
                controller.set_editor_overlays(button.is_active(), safe.is_active());
            });
        }
        {
            let controller = self.controller.clone();
            let grid = self.grid_toggle.clone();
            self.safe_toggle.connect_toggled(move |button| {
                controller.set_editor_overlays(grid.is_active(), button.is_active());
            });
        }

        {
            let controller = self.controller.clone();
            let list = self.playlist_items.clone();
            let dropdown = self.playlist_dropdown.clone();
            let updating = self.updating.clone();
            self.playlist_dropdown.connect_selected_notify(move |_| {
                if *updating.borrow() {
                    return;
                }
                let index = dropdown.selected() as usize;
                if let Some((id, _)) = list.borrow().get(index)
                    && !id.is_empty()
                {
                    controller.set_preview_playlist(id);
                }
            });
        }

        {
            let controller = self.controller.clone();
            let spin = self.duration_spin.clone();
            let updating = self.updating.clone();
            self.playlist_list.connect_row_selected(move |_, row| {
                if *updating.borrow() {
                    return;
                }
                let Some(row) = row else {
                    return;
                };
                let index = row.index() as usize;
                let playlist_id = controller.preview_state().playlist_id;
                let duration = controller
                    .project_clone()
                    .playlist(&playlist_id)
                    .map(|playlist| playlist.duration_at(index))
                    .unwrap_or(0);
                *updating.borrow_mut() = true;
                spin.set_value(f64::from(duration));
                *updating.borrow_mut() = false;
                controller.set_preview_index(index);
            });
        }

        {
            let controller = self.controller.clone();
            let updating = self.updating.clone();
            self.duration_spin.connect_value_changed(move |spin| {
                if *updating.borrow() {
                    return;
                }
                let index = controller.preview_state().index;
                let playlist_id = controller.preview_state().playlist_id;
                let value = spin.value() as u32;
                controller.mutate(Some("playlist-duration"), |project| {
                    if let Some(playlist) = project.playlist_mut(&playlist_id)
                        && let Some(item) = playlist.items.get_mut(index)
                    {
                        item.duration_secs = value;
                    }
                });
            });
        }

        {
            let selected = self.selected_asset.clone();
            self.assets_list.connect_row_selected(move |_, row| {
                let asset = row.map(|row| row.widget_name().to_string());
                *selected.borrow_mut() = asset.filter(|name| !name.is_empty());
            });
        }
    }

    fn refresh_playlist_dropdown(&self) {
        let playlists = self.controller.playlists();
        let desired: String = playlists
            .iter()
            .map(|(id, name)| format!("{id}:{name}"))
            .collect::<Vec<_>>()
            .join("|");
        let current: String = self
            .playlist_items
            .borrow()
            .iter()
            .map(|(id, name)| format!("{id}:{name}"))
            .collect::<Vec<_>>()
            .join("|");
        if current == desired {
            let active = self.controller.preview_state().playlist_id;
            if let Some(index) = playlists.iter().position(|(id, _)| id == &active)
                && self.playlist_dropdown.selected() != index as u32
            {
                *self.updating.borrow_mut() = true;
                self.playlist_dropdown.set_selected(index as u32);
                *self.updating.borrow_mut() = false;
            }
            return;
        }
        *self.playlist_items.borrow_mut() = playlists.clone();
        let strings: Vec<String> = playlists.iter().map(|(_, name)| name.clone()).collect();
        let refs: Vec<&str> = strings.iter().map(|s| s.as_str()).collect();
        *self.updating.borrow_mut() = true;
        self.playlist_dropdown
            .set_model(Some(&gtk::StringList::new(&refs)));
        let active = self.controller.preview_state().playlist_id;
        if let Some(index) = playlists.iter().position(|(id, _)| id == &active) {
            self.playlist_dropdown.set_selected(index as u32);
        }
        *self.updating.borrow_mut() = false;
    }
}

fn layer_kinds() -> Vec<(&'static str, LayerKind)> {
    vec![
        ("layer.type.text", LayerKind::Text(Default::default())),
        ("layer.type.clock", LayerKind::Clock(ClockProps::default())),
        ("layer.type.timer", LayerKind::Timer(TimerProps::default())),
        ("layer.type.eta", LayerKind::Eta(EtaProps::default())),
        ("layer.type.image", LayerKind::Image(ImageProps::default())),
        ("layer.type.video", LayerKind::Video(VideoProps::default())),
        (
            "layer.type.capture",
            LayerKind::Capture(CaptureProps::default()),
        ),
        ("layer.type.queue", LayerKind::Queue(QueueProps::default())),
        (
            "layer.type.ticker",
            LayerKind::Ticker(TickerProps::default()),
        ),
        (
            "layer.type.pattern",
            LayerKind::Pattern(PatternProps::default()),
        ),
    ]
}

/// Import files into `assets/`; returns stored relative paths.
pub fn import_paths(
    controller: &Rc<Controller>,
    overlay: &adw::ToastOverlay,
    paths: &[PathBuf],
) -> Vec<String> {
    let Some(dir) = controller.project_dir() else {
        return Vec::new();
    };
    let (imported, skipped) = assets::import_files(&dir, paths);
    if !imported.is_empty() {
        widgets::toast(
            overlay,
            &t!("assets.imported", count = imported.len()).to_string(),
        );
    }
    if !skipped.is_empty() {
        widgets::toast(
            overlay,
            &t!("assets.skipped", count = skipped.len()).to_string(),
        );
    }
    controller.mark_assets_changed();
    imported
}

/// Create an image or video layer for a freshly imported asset at a drop point.
pub fn create_media_layer(controller: &Rc<Controller>, asset: &str, dx: f64, dy: f64) {
    let kind = assets::kind_of(asset);
    let is_video = kind == assets::AssetKind::Video;
    let project = controller.project_clone();
    let width = project.design.width_f();
    let height = project.design.height_f();
    let size = if is_video {
        (width * 0.5, height * 0.5)
    } else {
        (width * 0.3, height * 0.3)
    };
    let rect = Rect::new(
        (dx - size.0 / 2.0).clamp(0.0, width - size.0),
        (dy - size.1 / 2.0).clamp(0.0, height - size.1),
        size.0,
        size.1,
    );
    let layer_kind = if is_video {
        LayerKind::Video(VideoProps {
            asset: Some(asset.to_string()),
            ..VideoProps::default()
        })
    } else {
        LayerKind::Image(ImageProps {
            asset: Some(asset.to_string()),
            ..ImageProps::default()
        })
    };
    let name = std::path::Path::new(asset)
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_else(|| asset.to_string());
    let mut layer = crate::model::Layer::new(layer_kind, name, rect);
    layer.normalize(width, height);
    let layer_id = layer.id.clone();
    let Some(scene_id) = controller.editing_scene_id() else {
        return;
    };
    controller.mutate(Some("layer-add-media"), |project| {
        if let Some(scene) = project.scene_mut(&scene_id) {
            scene.layers.push(layer);
        }
    });
    controller.select_layer(Some(&layer_id));
}

/// File chooser helper with an extension filter.
pub fn pick_files(
    overlay: &adw::ToastOverlay,
    title: &str,
    extensions: &[&str],
    multiple: bool,
    on_files: impl Fn(Vec<PathBuf>) + 'static,
) {
    let dialog = gtk::FileDialog::builder()
        .title(title.to_string())
        .modal(true)
        .build();
    let filter = gtk::FileFilter::new();
    filter.set_name(Some(title));
    for extension in extensions {
        filter.add_suffix(extension);
    }
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    dialog.set_filters(Some(&filters));
    dialog.set_default_filter(Some(&filter));

    let parent = overlay
        .root()
        .and_then(|root| root.downcast::<gtk::Window>().ok());
    if multiple {
        dialog.open_multiple(parent.as_ref(), gio::Cancellable::NONE, move |result| {
            let Ok(files) = result else {
                return;
            };
            let paths: Vec<PathBuf> = (0..files.n_items())
                .filter_map(|index| files.item(index))
                .filter_map(|item| item.downcast::<gio::File>().ok())
                .filter_map(|file| file.path())
                .collect();
            if !paths.is_empty() {
                on_files(paths);
            }
        });
    } else {
        dialog.open(parent.as_ref(), gio::Cancellable::NONE, move |result| {
            let Ok(file) = result else {
                return;
            };
            if let Some(path) = file.path() {
                on_files(vec![path]);
            }
        });
    }
}

impl Page for DesignPage {
    fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    fn refresh(&self) {
        if *self.updating.borrow() {
            return;
        }
        *self.updating.borrow_mut() = true;
        self.refresh_scenes();
        self.refresh_playlist_dropdown();
        self.refresh_playlist_items();
        self.refresh_assets();
        let editing = self.controller.editing();
        self.grid_toggle.set_active(editing.grid);
        self.safe_toggle.set_active(editing.safe_area);
        *self.updating.borrow_mut() = false;
        self.inspector.refresh();
    }
}

impl DesignPage {
    fn refresh_scenes(&self) {
        let project = self.controller.project_clone();
        let selected = self.controller.editing_scene_id();
        let program = self.controller.program();
        let signature: String = project
            .scenes
            .iter()
            .map(|scene| format!("{}:{}", scene.id, scene.name))
            .chain(std::iter::once(
                program.scene_id.clone().unwrap_or_default(),
            ))
            .collect::<Vec<_>>()
            .join("|");
        if *self.scenes_signature.borrow() == signature {
            return;
        }
        *self.scenes_signature.borrow_mut() = signature;
        while let Some(child) = self.scenes_list.first_child() {
            self.scenes_list.remove(&child);
        }
        for scene in &project.scenes {
            let row = gtk::ListBoxRow::new();
            row.set_widget_name(&scene.id);
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            line.set_margin_top(4);
            line.set_margin_bottom(4);
            line.set_margin_start(6);
            line.set_margin_end(6);
            let label = gtk::Label::new(Some(&scene.name));
            label.set_xalign(0.0);
            label.set_hexpand(true);
            line.append(&label);
            if program.scene_id.as_deref() == Some(scene.id.as_str())
                && matches!(
                    program.source,
                    ProgramSource::Playlist { .. } | ProgramSource::Manual { .. }
                )
            {
                let badge = gtk::Label::new(Some(&t!("show.on_air").to_string()));
                badge.add_css_class("signifiage-onair");
                line.append(&badge);
            }
            row.set_child(Some(&line));
            self.scenes_list.append(&row);
            if selected.as_deref() == Some(scene.id.as_str()) {
                self.scenes_list.select_row(Some(&row));
            }
        }
    }

    fn refresh_playlist_items(&self) {
        let playlist_id = self.controller.preview_state().playlist_id;
        let project = self.controller.project_clone();
        let preview = self.controller.preview_state();
        let program = self.controller.program();
        let Some(playlist) = project.playlist(&playlist_id) else {
            return;
        };
        let signature: String = playlist
            .items
            .iter()
            .map(|item| format!("{}:{}", item.scene_id, item.duration_secs))
            .chain(std::iter::once(format!("preview{}", preview.index)))
            .chain(std::iter::once(format!(
                "program{:?}",
                program.source.playlist_index()
            )))
            .collect::<Vec<_>>()
            .join("|");
        if *self.playlist_signature.borrow() == signature {
            return;
        }
        *self.playlist_signature.borrow_mut() = signature;
        while let Some(child) = self.playlist_list.first_child() {
            self.playlist_list.remove(&child);
        }
        for (index, item) in playlist.items.iter().enumerate() {
            let name = project
                .scene(&item.scene_id)
                .map(|scene| scene.name.clone())
                .unwrap_or_else(|| t!("scene.missing").to_string());
            let row = gtk::ListBoxRow::new();
            row.set_widget_name(&index.to_string());
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            line.set_margin_top(3);
            line.set_margin_bottom(3);
            line.set_margin_start(6);
            line.set_margin_end(6);
            let mut text = format!("{:02}  {}", index + 1, name);
            if item.duration_secs > 0 {
                text.push_str(&format!(" · {}s", item.duration_secs));
            }
            let label = gtk::Label::new(Some(&text));
            label.set_xalign(0.0);
            label.set_hexpand(true);
            line.append(&label);
            row.set_child(Some(&line));
            self.playlist_list.append(&row);
            if preview.index == index {
                self.playlist_list.select_row(Some(&row));
            }
        }
    }

    fn refresh_assets(&self) {
        let Some(dir) = self.controller.project_dir() else {
            return;
        };
        let entries = assets::list(&dir);
        let signature: String = entries
            .iter()
            .map(|entry| format!("{}:{}", entry.rel, entry.bytes))
            .collect::<Vec<_>>()
            .join("|");
        if *self.assets_signature.borrow() == signature {
            return;
        }
        *self.assets_signature.borrow_mut() = signature;
        while let Some(child) = self.assets_list.first_child() {
            self.assets_list.remove(&child);
        }
        *self.selected_asset.borrow_mut() = None;
        if entries.is_empty() {
            let row = gtk::ListBoxRow::new();
            row.set_selectable(false);
            row.set_activatable(false);
            let label = widgets::dim_label(&t!("assets.empty").to_string());
            label.set_margin_top(4);
            label.set_margin_bottom(4);
            label.set_margin_start(6);
            row.set_child(Some(&label));
            self.assets_list.append(&row);
            return;
        }
        for entry in entries {
            let row = gtk::ListBoxRow::new();
            row.set_widget_name(&entry.rel);
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            line.set_margin_top(3);
            line.set_margin_bottom(3);
            line.set_margin_start(6);
            line.set_margin_end(6);
            let icon = match entry.kind {
                assets::AssetKind::Image => "image-x-generic-symbolic",
                assets::AssetKind::Video => "video-x-generic-symbolic",
                assets::AssetKind::Audio => "audio-x-generic-symbolic",
                assets::AssetKind::Other => "text-x-generic-symbolic",
            };
            line.append(&gtk::Image::from_icon_name(icon));
            let label = gtk::Label::new(Some(&entry.name));
            label.set_xalign(0.0);
            label.set_hexpand(true);
            label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            line.append(&label);
            row.set_child(Some(&line));
            row.set_tooltip_text(Some(&entry.rel));
            self.assets_list.append(&row);
        }
    }
}
