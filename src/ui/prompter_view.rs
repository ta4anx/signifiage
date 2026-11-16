//! Teleprompter output widget.
//!
//! `PrompterView` draws the prompter text with Pango, scrolling it upwards at the
//! configured speed. Scroll state lives in a `PrompterRuntime` that can be shared
//! by several views (the page preview and the prompter output window), so both
//! stay in sync. The offset is derived from the wall clock while running, which
//! keeps multiple views coherent and is straightforward to test with a fake
//! clock through [`PrompterRuntime::offset_at`].

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use gtk::glib;
use gtk::graphene;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk4 as gtk;

use crate::model::Prompter;
use crate::model::layer::{TextAlign, TextStyle};
use crate::ui::scene_view::{full_rect, gdk_rgba, layout_size, text_layout};

/// Shared, clock-driven scroll state for one or more prompter views.
pub struct PrompterRuntime {
    base_offset: Cell<f64>,
    started: Cell<Option<Instant>>,
    speed: Cell<f64>,
    running: Cell<bool>,
    views: RefCell<Vec<glib::WeakRef<PrompterView>>>,
}

impl PrompterRuntime {
    pub fn new(speed: f64) -> Rc<Self> {
        Rc::new(Self {
            base_offset: Cell::new(0.0),
            started: Cell::new(None),
            speed: Cell::new(if speed.is_finite() && speed > 0.0 {
                speed
            } else {
                45.0
            }),
            running: Cell::new(false),
            views: RefCell::new(Vec::new()),
        })
    }

    /// Current scroll offset in pixels.
    pub fn offset(&self) -> f64 {
        self.offset_at(Instant::now())
    }

    /// Offset at an arbitrary instant; exposed so tests can use a fake clock.
    pub fn offset_at(&self, now: Instant) -> f64 {
        let mut value = self.base_offset.get();
        if self.running.get()
            && let Some(started) = self.started.get()
            && let Some(elapsed) = now.checked_duration_since(started)
        {
            value += elapsed.as_secs_f64() * self.speed.get();
        }
        value.max(0.0)
    }

    pub fn is_running(&self) -> bool {
        self.running.get()
    }

    pub fn speed(&self) -> f64 {
        self.speed.get()
    }

    /// Change the speed without jumping: the current offset becomes the new base.
    pub fn set_speed(&self, speed: f64) {
        if !speed.is_finite() || speed <= 0.0 || (speed - self.speed.get()).abs() < f64::EPSILON {
            return;
        }
        self.set_speed_at(speed, Instant::now());
    }

    fn set_speed_at(&self, speed: f64, now: Instant) {
        let current = self.offset_at(now);
        self.base_offset.set(current);
        self.started
            .set(if self.running.get() { Some(now) } else { None });
        self.speed.set(speed);
        self.notify_views();
    }

    pub fn start(&self) {
        self.start_at(Instant::now());
    }

    fn start_at(&self, now: Instant) {
        if self.running.get() {
            return;
        }
        self.running.set(true);
        self.started.set(Some(now));
        self.notify_views();
    }

    pub fn pause(&self) {
        self.pause_at(Instant::now());
    }

    fn pause_at(&self, now: Instant) {
        if !self.running.get() {
            return;
        }
        self.base_offset.set(self.offset_at(now));
        self.running.set(false);
        self.started.set(None);
        self.notify_views();
    }

    /// Jump back to the top, keeping the running/paused state.
    pub fn reset(&self) {
        self.reset_at(Instant::now());
    }

    fn reset_at(&self, now: Instant) {
        self.base_offset.set(0.0);
        self.started
            .set(if self.running.get() { Some(now) } else { None });
        self.notify_views();
    }

    pub fn set_running(&self, running: bool) {
        if running {
            self.start();
        } else {
            self.pause();
        }
    }

    /// Move the offset to an absolute position (used by reset and the page).
    pub fn set_offset(&self, px: f64) {
        self.set_offset_at(px, Instant::now());
    }

    fn set_offset_at(&self, px: f64, now: Instant) {
        self.base_offset.set(px.max(0.0));
        self.started
            .set(if self.running.get() { Some(now) } else { None });
        self.notify_views();
    }

    fn attach(&self, view: &PrompterView) {
        let mut views = self.views.borrow_mut();
        views.retain(|weak| weak.upgrade().is_some());
        views.push(view.downgrade());
    }

    fn notify_views(&self) {
        let views: Vec<PrompterView> = self
            .views
            .borrow()
            .iter()
            .filter_map(|weak| weak.upgrade())
            .collect();
        for view in views {
            view.on_runtime_change();
        }
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct PrompterView {
        pub runtime: RefCell<Option<Rc<PrompterRuntime>>>,
        pub settings: RefCell<Prompter>,
        pub text_height: Cell<f64>,
        pub tick: RefCell<Option<gtk::TickCallbackId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PrompterView {
        const NAME: &'static str = "SignifiagePrompterView";
        type Type = super::PrompterView;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for PrompterView {}

    impl WidgetImpl for PrompterView {
        fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            (0, 120, -1, -1)
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let width = f64::from(widget.width());
            let height = f64::from(widget.height());
            if width <= 0.0 || height <= 0.0 {
                return;
            }

            let settings = self.settings.borrow();
            let offset = self
                .runtime
                .borrow()
                .as_ref()
                .map(|runtime| runtime.offset())
                .unwrap_or(0.0);

            snapshot.save();
            // Mirror around the widget centre when requested.
            if settings.mirror_h || settings.mirror_v {
                let sx = if settings.mirror_h { -1.0 } else { 1.0 };
                let sy = if settings.mirror_v { -1.0 } else { 1.0 };
                let cx = (width / 2.0) as f32;
                let cy = (height / 2.0) as f32;
                snapshot.translate(&graphene::Point::new(cx, cy));
                snapshot.scale(sx as f32, sy as f32);
                snapshot.translate(&graphene::Point::new(-cx, -cy));
            }

            snapshot.append_color(&gdk_rgba(settings.bg_rgba()), &full_rect(width, height));
            snapshot.push_clip(&full_rect(width, height));

            let text = if settings.has_script() {
                settings.text.clone()
            } else {
                rust_i18n::t!("prompter.no_script").to_string()
            };
            let column = (width * settings.width_pct.clamp(10.0, 100.0) / 100.0).max(1.0);
            let style = TextStyle {
                family: settings.font_family.clone(),
                size: settings.font_size,
                bold: false,
                italic: false,
                color: settings.fg.clone(),
                align: TextAlign::Center,
                line_spacing: settings.line_spacing,
                uppercase: false,
            };
            let layout = text_layout(&widget.create_pango_context(), &text, &style, column, 1.0);
            let (_text_width, text_height) = layout_size(&layout);
            self.text_height.set(text_height);

            let x = ((width - column) / 2.0).max(0.0);
            let y = height * 0.12 - offset;
            snapshot.translate(&graphene::Point::new(x as f32, y as f32));
            snapshot.append_layout(&layout, &gdk_rgba(settings.fg_rgba()));

            snapshot.pop();
            snapshot.restore();
        }
    }
}

glib::wrapper! {
    pub struct PrompterView(ObjectSubclass<imp::PrompterView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for PrompterView {
    fn default() -> Self {
        Self::new()
    }
}

impl PrompterView {
    pub fn new() -> Self {
        Self::with_runtime(PrompterRuntime::new(Prompter::default().speed))
    }

    /// Build a view driven by an existing runtime (shared by page and output).
    pub fn with_runtime(runtime: Rc<PrompterRuntime>) -> Self {
        let view: Self = glib::Object::builder().build();
        view.set_hexpand(true);
        view.set_vexpand(true);
        view.set_size_request(240, 135);
        view.imp().runtime.replace(Some(runtime.clone()));
        view.imp().settings.replace(Prompter::default());
        runtime.attach(&view);
        view
    }

    /// The runtime driving this view, if any.
    pub fn runtime(&self) -> Option<Rc<PrompterRuntime>> {
        self.imp().runtime.borrow().clone()
    }

    /// Draw the text with the given family, size, colours, mirroring and width.
    pub fn set_settings(&self, prompter: &Prompter) {
        let mut settings = prompter.clone();
        settings.normalize();
        *self.imp().settings.borrow_mut() = settings.clone();
        if let Some(runtime) = self.runtime() {
            runtime.set_speed(settings.speed);
        }
        self.queue_draw();
    }

    pub fn set_offset(&self, px: f64) {
        if let Some(runtime) = self.runtime() {
            runtime.set_offset(px);
        }
        self.queue_draw();
    }

    pub fn offset(&self) -> f64 {
        self.runtime()
            .map(|runtime| runtime.offset())
            .unwrap_or(0.0)
    }

    pub fn set_running(&self, running: bool) {
        if let Some(runtime) = self.runtime() {
            runtime.set_running(running);
        }
        if running {
            self.ensure_tick();
        } else {
            self.stop_tick();
        }
        self.queue_draw();
    }

    /// Whether the text has scrolled to its end.
    pub fn at_end(&self) -> bool {
        self.end_offset()
            .is_some_and(|limit| self.offset() >= limit)
    }

    /// Scroll offset at which the end of the text has been reached, if known.
    fn end_offset(&self) -> Option<f64> {
        let text_height = self.imp().text_height.get();
        if text_height <= 0.0 {
            return None;
        }
        let limit = text_height - f64::from(self.height()) / 2.0;
        Some(limit.max(0.0))
    }

    pub(crate) fn on_runtime_change(&self) {
        let running = self
            .runtime()
            .map(|runtime| runtime.is_running())
            .unwrap_or(false);
        if running {
            self.ensure_tick();
        } else {
            self.stop_tick();
        }
        self.queue_draw();
    }

    fn ensure_tick(&self) {
        let imp = self.imp();
        if imp.tick.borrow().is_some() {
            return;
        }
        let Some(runtime) = self.runtime() else {
            return;
        };
        if !runtime.is_running() {
            return;
        }
        let weak = self.downgrade();
        let id = self.add_tick_callback(move |_, _| match weak.upgrade() {
            Some(view) => view.on_tick(),
            None => glib::ControlFlow::Break,
        });
        imp.tick.replace(Some(id));
    }

    fn stop_tick(&self) {
        if let Some(id) = self.imp().tick.borrow_mut().take() {
            id.remove();
        }
    }

    fn on_tick(&self) -> glib::ControlFlow {
        let Some(runtime) = self.runtime() else {
            self.imp().tick.replace(None);
            return glib::ControlFlow::Break;
        };
        if !runtime.is_running() {
            self.imp().tick.replace(None);
            return glib::ControlFlow::Break;
        }
        if let Some(limit) = self.end_offset()
            && runtime.offset() >= limit
        {
            // Drop the id first: pausing notifies every view, and removing the
            // callback we are currently inside is not allowed.
            self.imp().tick.replace(None);
            runtime.pause();
            runtime.set_offset(limit);
            self.queue_draw();
            return glib::ControlFlow::Break;
        }
        self.queue_draw();
        glib::ControlFlow::Continue
    }
}

/// Build a prompter output widget sharing `runtime` with the page preview.
///
/// The output window manager currently hosts a `SceneView` per role; the
/// integration owner can attach this widget instead so the prompter window shows
/// the same scroll position as the page.
pub fn make_output_widget(runtime: Rc<PrompterRuntime>, prompter: &Prompter) -> gtk::Widget {
    let view = PrompterView::with_runtime(runtime);
    view.set_settings(prompter);
    view.upcast()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn runtime_advances_with_the_clock_and_pauses() {
        let t0 = Instant::now();
        let runtime = PrompterRuntime::new(45.0);
        assert_eq!(runtime.offset_at(t0), 0.0);

        runtime.start_at(t0);
        assert!(runtime.is_running());
        assert!((runtime.offset_at(t0 + Duration::from_secs(2)) - 90.0).abs() < 1e-6);

        runtime.pause_at(t0 + Duration::from_secs(2));
        assert!(!runtime.is_running());
        // Paused time does not count.
        assert!((runtime.offset_at(t0 + Duration::from_secs(10)) - 90.0).abs() < 1e-6);

        runtime.start_at(t0 + Duration::from_secs(10));
        assert!((runtime.offset_at(t0 + Duration::from_secs(11)) - 135.0).abs() < 1e-6);
    }

    #[test]
    fn reset_returns_to_zero_and_keeps_running() {
        let t0 = Instant::now();
        let runtime = PrompterRuntime::new(60.0);
        runtime.start_at(t0);
        let later = t0 + Duration::from_secs(3);
        assert!((runtime.offset_at(later) - 180.0).abs() < 1e-6);
        runtime.reset_at(later);
        assert_eq!(runtime.offset_at(later), 0.0);
        assert!(runtime.is_running());
        assert!((runtime.offset_at(later + Duration::from_secs(1)) - 60.0).abs() < 1e-6);
    }

    #[test]
    fn speed_change_keeps_the_current_offset() {
        let t0 = Instant::now();
        let runtime = PrompterRuntime::new(45.0);
        runtime.start_at(t0);
        let at = t0 + Duration::from_secs(2);
        runtime.set_speed_at(100.0, at);
        assert!((runtime.offset_at(at) - 90.0).abs() < 1e-6);
        assert!((runtime.offset_at(at + Duration::from_millis(500)) - 140.0).abs() < 1e-6);
    }

    #[test]
    fn set_offset_is_clamped_to_zero() {
        let runtime = PrompterRuntime::new(45.0);
        runtime.set_offset(-20.0);
        assert_eq!(runtime.offset(), 0.0);
        runtime.set_offset(42.0);
        assert_eq!(runtime.offset(), 42.0);
    }
}
