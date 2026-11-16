//! Design canvas: a `SceneView` inside a scrolled area with selection, dragging,
//! corner-handle resizing, snapping and editor zoom.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::graphene;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::model::layer::Rect;
use crate::services::controller::{Controller, ViewRole};
use crate::ui::render::{SceneView, ViewMode};

const HANDLE: f64 = 12.0;
const MIN_SIZE: f64 = 20.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(Debug, Clone)]
struct DragState {
    layer_id: String,
    corner: Option<Corner>,
    origin: Rect,
    design: (f64, f64),
}

pub struct Canvas {
    /// Container to embed: switches between a plain fit view and a scrolled one.
    pub container: gtk::Box,
    pub scrolled: gtk::ScrolledWindow,
    pub fit_box: gtk::Box,
    pub stack: gtk::Stack,
    pub view: SceneView,
    controller: Rc<Controller>,
    drag: RefCell<Option<DragState>>,
}

impl Canvas {
    pub fn new(controller: Rc<Controller>) -> Rc<Self> {
        let view = SceneView::new(ViewMode::Editor);
        view.set_hexpand(true);
        view.set_vexpand(true);

        let scrolled = gtk::ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .build();
        scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Never);
        scrolled.add_css_class("scene-canvas");

        // Fit mode hosts the view directly so it fills the page; explicit zoom
        // wraps it in the scrolled window with a real size request.
        let fit_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        fit_box.set_hexpand(true);
        fit_box.set_vexpand(true);
        fit_box.append(&view);

        let stack = gtk::Stack::new();
        stack.set_hexpand(true);
        stack.set_vexpand(true);
        stack.add_named(&fit_box, Some("fit"));
        stack.add_named(&scrolled, Some("zoom"));
        stack.set_visible_child_name("fit");

        let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
        container.set_hexpand(true);
        container.set_vexpand(true);
        container.append(&stack);

        let canvas = Rc::new(Self {
            container,
            scrolled,
            fit_box,
            stack,
            view: view.clone(),
            controller: controller.clone(),
            drag: RefCell::new(None),
        });

        controller.attach_view(ViewRole::Editor, &view);
        canvas.install_clicks(&view);
        canvas.install_drags(&view);
        canvas
    }

    /// Editor zoom: 0 = fit, otherwise a scale factor (0.5, 0.75, 1.0).
    pub fn set_zoom(&self, zoom: f64) {
        self.view.set_zoom(zoom);
        let in_scrolled = self
            .view
            .parent()
            .is_some_and(|parent| parent.eq(&self.scrolled));
        if zoom > 0.0 {
            let project = self.controller.project_clone();
            let width = (f64::from(project.design.width) * zoom).round() as i32;
            let height = (f64::from(project.design.height) * zoom).round() as i32;
            self.view.set_size_request(width, height);
            if !in_scrolled {
                if let Some(parent) = self.view.parent() {
                    parent.downcast::<gtk::Box>().ok().map(|box_| {
                        box_.remove(&self.view);
                    });
                }
                self.scrolled.set_child(Some(&self.view));
            }
            self.scrolled
                .set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
            self.stack.set_visible_child_name("zoom");
        } else {
            if in_scrolled {
                self.scrolled.set_child(None::<&gtk::Widget>);
                self.fit_box.append(&self.view);
            }
            self.scrolled
                .set_policy(gtk::PolicyType::Never, gtk::PolicyType::Never);
            self.view.set_size_request(640, 360);
            self.view.set_size_request(-1, -1);
            self.stack.set_visible_child_name("fit");
        }
        self.queue_redraw();
    }

    fn queue_redraw(&self) {
        self.view.queue_draw();
    }

    fn install_clicks(self: &Rc<Self>, view: &SceneView) {
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_PRIMARY);
        let weak = Rc::downgrade(self);
        click.connect_pressed(move |_, _presses, x, y| {
            let Some(canvas) = weak.upgrade() else {
                return;
            };
            let (dx, dy) = canvas.view.view_to_design(x, y);
            let scene = canvas.controller.editing_scene();
            let Some(scene) = scene else {
                return;
            };
            if let Some(layer) = scene.hit_layer(dx, dy) {
                let id = layer.id.clone();
                canvas.controller.select_layer(Some(&id));
            } else {
                canvas.controller.select_layer(None);
            }
            canvas.view.grab_focus();
        });
        view.add_controller(click);
    }

    fn install_drags(self: &Rc<Self>, view: &SceneView) {
        let drag = gtk::GestureDrag::new();
        drag.set_button(gtk::gdk::BUTTON_PRIMARY);

        let weak = Rc::downgrade(self);
        drag.connect_drag_begin(move |_, x, y| {
            let Some(canvas) = weak.upgrade() else {
                return;
            };
            let Some(scene) = canvas.controller.editing_scene() else {
                return;
            };
            let selected = canvas.controller.selected_layer();
            let (dx, dy) = canvas.view.view_to_design(x, y);
            let design = (
                f64::from(canvas.controller.project_clone().design.width),
                f64::from(canvas.controller.project_clone().design.height),
            );

            // A drag that starts on a corner handle of the selected layer resizes.
            if let Some(selected_id) = selected
                && let Some(layer) = scene.layer(&selected_id)
            {
                let corner = canvas.corner_at(hit_test(layer.rect, dx, dy), x, y);
                if let Some(corner) = corner {
                    *canvas.drag.borrow_mut() = Some(DragState {
                        layer_id: selected_id,
                        corner: Some(corner),
                        origin: layer.rect,
                        design,
                    });
                    return;
                }
            }

            if let Some(layer) = scene.hit_layer(dx, dy) {
                let id = layer.id.clone();
                canvas.controller.select_layer(Some(&id));
                *canvas.drag.borrow_mut() = Some(DragState {
                    layer_id: id,
                    corner: None,
                    origin: layer.rect,
                    design,
                });
            }
        });

        let weak = Rc::downgrade(self);
        drag.connect_drag_update(move |gesture, offset_x, offset_y| {
            let Some(canvas) = weak.upgrade() else {
                return;
            };
            let state = canvas.drag.borrow().clone();
            let Some(state) = state else {
                return;
            };
            let scale = canvas.view.geometry().sx.max(0.0001);
            let dx = offset_x / scale;
            let dy = offset_y / scale;
            let snap = !gesture
                .current_event_state()
                .contains(gtk::gdk::ModifierType::SHIFT_MASK);
            let grid = f64::from(canvas.controller.project_clone().design.grid.max(1));

            let mut rect = state.origin;
            match state.corner {
                None => {
                    rect.x = state.origin.x + dx;
                    rect.y = state.origin.y + dy;
                    if snap {
                        rect.x = (rect.x / grid).round() * grid;
                        rect.y = (rect.y / grid).round() * grid;
                    }
                    rect.x = rect.x.clamp(0.0, (state.design.0 - rect.w).max(0.0));
                    rect.y = rect.y.clamp(0.0, (state.design.1 - rect.h).max(0.0));
                }
                Some(corner) => {
                    let (mut left, mut top) = (state.origin.x, state.origin.y);
                    let (mut right, mut bottom) = (
                        state.origin.x + state.origin.w,
                        state.origin.y + state.origin.h,
                    );
                    match corner {
                        Corner::TopLeft => {
                            left += dx;
                            top += dy;
                        }
                        Corner::TopRight => {
                            right += dx;
                            top += dy;
                        }
                        Corner::BottomLeft => {
                            left += dx;
                            bottom += dy;
                        }
                        Corner::BottomRight => {
                            right += dx;
                            bottom += dy;
                        }
                    }
                    if snap {
                        left = (left / grid).round() * grid;
                        top = (top / grid).round() * grid;
                        right = (right / grid).round() * grid;
                        bottom = (bottom / grid).round() * grid;
                    }
                    left = left.clamp(0.0, state.design.0 - MIN_SIZE);
                    top = top.clamp(0.0, state.design.1 - MIN_SIZE);
                    right = right.clamp(left + MIN_SIZE, state.design.0);
                    bottom = bottom.clamp(top + MIN_SIZE, state.design.1);
                    rect = Rect::new(left, top, right - left, bottom - top);
                }
            }
            canvas
                .controller
                .set_layer_rect(&state.layer_id, rect, Some("layer-drag"));
        });

        let weak = Rc::downgrade(self);
        drag.connect_drag_end(move |_, _, _| {
            if let Some(canvas) = weak.upgrade() {
                canvas.drag.borrow_mut().take();
            }
        });

        view.add_controller(drag);
    }

    fn corner_at(&self, handle: Option<Corner>, x: f64, y: f64) -> Option<Corner> {
        let _ = (self, x, y);
        handle
    }
}

fn hit_test(rect: Rect, dx: f64, dy: f64) -> Option<Corner> {
    let near_left = (dx - rect.x).abs() <= HANDLE;
    let near_right = (dx - (rect.x + rect.w)).abs() <= HANDLE;
    let near_top = (dy - rect.y).abs() <= HANDLE;
    let near_bottom = (dy - (rect.y + rect.h)).abs() <= HANDLE;
    match (near_left, near_right, near_top, near_bottom) {
        (true, _, true, _) => Some(Corner::TopLeft),
        (_, true, true, _) => Some(Corner::TopRight),
        (true, _, _, true) => Some(Corner::BottomLeft),
        (_, true, _, true) => Some(Corner::BottomRight),
        _ => None,
    }
}

/// Convert a widget-space point to design space for tests and gestures.
pub fn to_design(view: &SceneView, x: f64, y: f64) -> (f64, f64) {
    view.view_to_design(x, y)
}

/// Rect of a layer in widget space, used by hit tests.
pub fn layer_view_rect(view: &SceneView, rect: Rect) -> graphene::Rect {
    let geometry = view.geometry();
    let (x, y) = geometry.to_view(rect.x, rect.y);
    graphene::Rect::new(
        x as f32,
        y as f32,
        (rect.w * geometry.sx) as f32,
        (rect.h * geometry.sy) as f32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corner_hit_testing() {
        let rect = Rect::new(100.0, 100.0, 200.0, 100.0);
        assert_eq!(hit_test(rect, 100.0, 100.0), Some(Corner::TopLeft));
        assert_eq!(hit_test(rect, 300.0, 100.0), Some(Corner::TopRight));
        assert_eq!(hit_test(rect, 100.0, 200.0), Some(Corner::BottomLeft));
        assert_eq!(hit_test(rect, 300.0, 200.0), Some(Corner::BottomRight));
        assert_eq!(hit_test(rect, 200.0, 150.0), None);
    }

    #[test]
    fn minimum_size_is_enforced_by_clamping() {
        let mut rect = Rect::new(10.0, 10.0, 1.0, 1.0);
        rect.clamp_to(1920.0, 1080.0);
        assert!(rect.w >= MIN_SIZE);
        assert!(rect.h >= MIN_SIZE);
    }
}
