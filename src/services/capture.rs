//! Capture source enumeration and resolution.
//!
//! Pure planning ([`plan_for`]) decides which GStreamer pipeline a capture layer
//! needs. Session-backed sources (portal screen capture) are resolved through
//! [`crate::services::portal`]; a source without an approved session reports
//! [`CaptureError::PortalPending`] instead of blocking the UI.
//!
//! [`test_window`] is the inspector's explicit, muted preview: its own
//! [`MediaHub`] so it never touches program audio or program capture sessions.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::{DeviceExt as _, DeviceMonitorExt as _, DeviceMonitorExtManual as _};
use gtk::glib;
use gtk::glib::object::ObjectExt as _;
use gtk::prelude::{ButtonExt, GtkWindowExt, WidgetExt};
use gtk4 as gtk;

use crate::config::CaptureBackend;
use crate::model::layer::{CaptureProps, CaptureSource, LayerKind};
use crate::model::project::{Project, Scene};
use crate::services::controller::Controller;
use crate::services::media::{self, MediaEvent, MediaHub, MediaSource, MediaSpec};
use crate::services::portal::{self, PortalError, PortalStream};
use crate::services::x11;
use crate::ui::render::{MediaProvider, SceneView, ViewMode};

/// One camera exposed by GStreamer or `/dev/video*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraDevice {
    /// Device node such as `/dev/video0`.
    pub path: String,
    /// Human-readable device name (`Camera (V4L2)` or the node path).
    pub name: String,
}

/// A ready-to-build capture pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapturePlan {
    /// `v4l2src` pipeline, already parameterised by the sink name.
    Camera { description: String },
    /// `ximagesrc` pipeline, already parameterised by the sink name.
    X11 { description: String },
    /// PipeWire stream from an approved portal session.
    Portal { fd: i32, node: u32, label: String },
}

impl CapturePlan {
    /// Pipeline description for a sink element name.
    ///
    /// Camera and X11 plans were built with the same sink, so it is ignored for
    /// them; the portal plan is formatted on demand.
    pub fn description(&self, sink: &str) -> String {
        match self {
            CapturePlan::Camera { description } | CapturePlan::X11 { description } => {
                description.clone()
            }
            CapturePlan::Portal { fd, node, .. } => media::portal_description(*fd, *node, sink),
        }
    }

    /// Source label for the placeholder and the inspector.
    pub fn label(&self) -> String {
        match self {
            CapturePlan::Camera { .. } => rust_i18n::t!("capture.type.camera").to_string(),
            CapturePlan::X11 { .. } => rust_i18n::t!("capture.type.x11_window").to_string(),
            CapturePlan::Portal { label, .. } => label.clone(),
        }
    }
}

/// Reasons a capture layer cannot be planned.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CaptureError {
    #[error("No camera is selected for this capture layer")]
    NoDevice,
    #[error("No X11 display is available; window capture needs an X session or the portal backend")]
    NoDisplay,
    #[error("The selected window is no longer available")]
    NoWindow,
    #[error("Screen capture needs approval before it can start")]
    PortalPending,
    #[error("The desktop portal is unavailable; screen capture needs xdg-desktop-portal")]
    PortalUnavailable,
    #[error("{0}")]
    Unsupported(String),
}

impl CaptureError {
    /// Localised, actionable text for a toast or an inline inspector error.
    pub fn message(&self) -> String {
        match self {
            CaptureError::NoDevice => rust_i18n::t!("capture.error.no_device").to_string(),
            CaptureError::NoDisplay => rust_i18n::t!("capture.error.no_display").to_string(),
            CaptureError::NoWindow => rust_i18n::t!("capture.pick_window").to_string(),
            CaptureError::PortalPending => {
                rust_i18n::t!("capture.error.portal_pending").to_string()
            }
            CaptureError::PortalUnavailable => {
                rust_i18n::t!("capture.error.portal_unavailable").to_string()
            }
            CaptureError::Unsupported(message) => message.clone(),
        }
    }
}

/// Cameras known to GStreamer, with a `/dev/video*` fallback for missing plugins.
///
/// Results are deduplicated by device path and sorted, so pickers are stable.
pub fn list_cameras() -> Vec<CameraDevice> {
    let mut out: Vec<CameraDevice> = Vec::new();
    if gst::init().is_ok() {
        let monitor = gst::DeviceMonitor::new();
        monitor.add_filter(Some("Video/Source"), None);
        if monitor.start().is_ok() {
            for device in monitor.devices() {
                let path = device
                    .properties()
                    .as_ref()
                    .and_then(|props| {
                        props
                            .get::<String>("device.path")
                            .or_else(|_| props.get::<String>("api.v4l2.path"))
                            .ok()
                    })
                    .filter(|path| !path.trim().is_empty());
                let Some(path) = path else {
                    continue;
                };
                if out.iter().any(|existing| existing.path == path) {
                    continue;
                }
                let name = device
                    .properties()
                    .as_ref()
                    .and_then(|props| props.get::<String>("device.name").ok())
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| path.clone());
                out.push(CameraDevice { path, name });
            }
            monitor.stop();
        }
    }
    for fallback in dev_video_nodes() {
        if !out.iter().any(|existing| existing.path == fallback.path) {
            out.push(fallback);
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn dev_video_nodes() -> Vec<CameraDevice> {
    let Ok(entries) = std::fs::read_dir("/dev") else {
        return Vec::new();
    };
    let mut nodes: Vec<CameraDevice> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("video") {
                return None;
            }
            let path = entry.path().to_string_lossy().to_string();
            Some(CameraDevice {
                name: path.clone(),
                path,
            })
        })
        .collect();
    nodes.sort_by(|a, b| a.path.cmp(&b.path));
    nodes
}

/// Resolve a capture layer's source into a pipeline plan.
///
/// Portal sources use the process-global approved-session cache; when nothing
/// has been approved yet this returns [`CaptureError::PortalPending`] so the UI
/// can start [`portal::request_async`] on a worker thread.
pub fn plan_for(
    source: &CaptureSource,
    backend: CaptureBackend,
    sink: &str,
) -> Result<CapturePlan, CaptureError> {
    plan_with_portal(source, backend, sink, x11::display_available(), None)
}

/// [`plan_for`] with the display check injected (tests, headless players) and an
/// optional already-approved portal stream.
pub fn plan_with_portal(
    source: &CaptureSource,
    backend: CaptureBackend,
    sink: &str,
    display_available: bool,
    portal_stream: Option<PortalStream>,
) -> Result<CapturePlan, CaptureError> {
    match source {
        CaptureSource::Camera {
            device,
            width,
            height,
            fps,
            ..
        } => {
            if device.trim().is_empty() {
                return Err(CaptureError::NoDevice);
            }
            Ok(CapturePlan::Camera {
                description: media::camera_description(device, *width, *height, *fps, sink),
            })
        }
        CaptureSource::X11Window {
            title,
            wm_class,
            xid,
        } => {
            let use_x11 = match backend {
                CaptureBackend::X11 => true,
                CaptureBackend::Portal => false,
                CaptureBackend::Auto => display_available,
            };
            if use_x11 {
                if !display_available {
                    return Err(CaptureError::NoDisplay);
                }
                let xid = if *xid != 0 {
                    *xid
                } else {
                    x11::resolve_window(*xid, title, wm_class).ok_or(CaptureError::NoWindow)?
                };
                Ok(CapturePlan::X11 {
                    description: media::x11_description(xid, sink),
                })
            } else {
                let label = if !title.trim().is_empty() {
                    title.clone()
                } else {
                    wm_class.clone()
                };
                portal_plan(source, label, portal_stream)
            }
        }
        CaptureSource::Portal { label, .. } => {
            if backend == CaptureBackend::X11 {
                return Err(CaptureError::Unsupported(
                    "This source needs the desktop portal: set the capture backend to Automatic or Desktop portal."
                        .to_string(),
                ));
            }
            portal_plan(source, label.clone(), portal_stream)
        }
    }
}

fn portal_plan(
    source: &CaptureSource,
    label: String,
    provided: Option<PortalStream>,
) -> Result<CapturePlan, CaptureError> {
    let stream = match provided {
        Some(stream) => stream,
        None => portal::stream_for_key(&source.pipeline_key()).map_err(|err| match err {
            PortalError::NotRequested => CaptureError::PortalPending,
            _ => CaptureError::PortalUnavailable,
        })?,
    };
    let label = if label.trim().is_empty() {
        stream.label
    } else {
        label
    };
    Ok(CapturePlan::Portal {
        fd: stream.fd,
        node: stream.node,
        label,
    })
}

/// Build the [`MediaSpec`] a capture layer needs, or the reason it cannot start.
///
/// Keyed by `CaptureSource::pipeline_key`, so an unchanged source keeps its
/// pipeline across scene refreshes.
pub fn media_spec_for(
    layer_id: &str,
    props: &CaptureProps,
    backend: CaptureBackend,
    sink: &str,
) -> Result<MediaSpec, CaptureError> {
    let plan = plan_for(&props.source, backend, sink)?;
    Ok(MediaSpec {
        layer_id: layer_id.to_string(),
        key: format!("capture|{}", props.source.pipeline_key()),
        source: MediaSource::Capture {
            description: plan.description(sink),
            sink_name: "sink".to_string(),
        },
    })
}

/// Start the portal picker for `source` on a worker thread.
///
/// The result arrives on the returned channel and, on success, the approved
/// session is already installed in the portal cache: the caller only needs to
/// retry [`plan_for`]. Poll with `try_recv` from the UI tick.
pub fn request_portal_source(
    source: &CaptureSource,
) -> Receiver<Result<PortalStream, PortalError>> {
    let restore_token = match source {
        CaptureSource::Portal { restore_token, .. } => restore_token.clone(),
        _ => None,
    };
    portal::request_async(source.pipeline_key(), restore_token)
}

/// A muted MediaHub exposed to a `SceneView` through `MediaProvider`.
struct TestMedia(Rc<RefCell<MediaHub>>);

impl MediaProvider for TestMedia {
    fn paintable(&self, layer_id: &str) -> Option<gtk::gdk::Paintable> {
        self.0.borrow().paintable(layer_id)
    }
}

struct CaptureLayer {
    scene: Scene,
    props: CaptureProps,
}

fn capture_layer(project: &Project, layer_id: &str) -> Option<CaptureLayer> {
    for scene in &project.scenes {
        for layer in &scene.layers {
            if layer.id != layer_id {
                continue;
            }
            if let LayerKind::Capture(props) = &layer.kind {
                return Some(CaptureLayer {
                    scene: scene.clone(),
                    props: props.clone(),
                });
            }
        }
    }
    None
}

/// Open the inspector's capture test window: 640×360, muted, own pipeline.
pub fn test_window(parent: &gtk::Window, controller: Rc<Controller>, layer_id: &str) {
    let project = controller.project_clone();
    let Some(layer) = capture_layer(&project, layer_id) else {
        controller.toast(&rust_i18n::t!("capture.unset").to_string());
        return;
    };
    let backend = controller.config().capture_backend;
    let asset_base = controller.project_dir();
    let sink = "gtk4paintablesink";

    let view = SceneView::new(ViewMode::Program);
    view.set_can_focus(false);
    view.set_focusable(false);
    view.set_design(
        project.design.width,
        project.design.height,
        "#000000",
        project.presentation.scale,
        0,
    );
    view.set_scene(Some(layer.scene.clone()));
    view.set_asset_base(asset_base);
    view.set_render_data(controller.render_data());

    let hub = Rc::new(RefCell::new(MediaHub::new()));
    if let Some(device) = controller.config().audio_sink {
        hub.borrow_mut().set_sink(Some(device));
    }
    hub.borrow_mut().set_muted(true);
    view.set_media_provider(Some(Rc::new(TestMedia(hub.clone()))));

    let window = gtk::Window::builder()
        .transient_for(parent)
        .title(rust_i18n::t!("capture.test_window").to_string())
        .default_width(640)
        .default_height(360)
        .build();
    let header = gtk::HeaderBar::new();
    let close = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .tooltip_text(rust_i18n::t!("common.close").to_string())
        .build();
    close.connect_clicked({
        let window = window.clone();
        move |_| window.close()
    });
    header.pack_end(&close);
    window.set_titlebar(Some(&header));
    window.set_child(Some(&view));

    let alive = Rc::new(Cell::new(true));
    let spec: Rc<RefCell<Option<MediaSpec>>> = Rc::new(RefCell::new(None));
    let portal_rx: Rc<RefCell<Option<Receiver<Result<PortalStream, PortalError>>>>> =
        Rc::new(RefCell::new(None));
    let portal_requested = Rc::new(Cell::new(false));
    let warned = Rc::new(Cell::new(false));
    let ticks = Rc::new(Cell::new(0u32));

    let _tick = glib::timeout_add_local(Duration::from_millis(100), {
        let hub = hub.clone();
        let view = view.downgrade();
        let controller = controller.clone();
        let props = layer.props.clone();
        let layer_id = layer_id.to_string();
        let alive = alive.clone();
        let spec = spec.clone();
        let portal_rx = portal_rx.clone();
        let portal_requested = portal_requested.clone();
        let warned = warned.clone();
        let ticks = ticks.clone();
        move || {
            if !alive.get() {
                return glib::ControlFlow::Break;
            }
            if spec.borrow().is_none() {
                let outcome = {
                    let mut rx = portal_rx.borrow_mut();
                    match rx.as_ref().map(Receiver::try_recv) {
                        Some(Ok(outcome)) => {
                            rx.take();
                            Some(outcome)
                        }
                        Some(Err(TryRecvError::Empty)) => None,
                        Some(Err(TryRecvError::Disconnected)) => {
                            rx.take();
                            Some(Err(PortalError::WorkerStopped))
                        }
                        None => None,
                    }
                };
                if let Some(Err(err)) = outcome {
                    controller.toast(&err.message());
                }
                match media_spec_for(&layer_id, &props, backend, sink) {
                    Ok(built) => {
                        hub.borrow_mut()
                            .reconcile(std::slice::from_ref(&built), false);
                        if let Some(view) = view.upgrade() {
                            view.refresh_media_links();
                        }
                        spec.borrow_mut().replace(built);
                    }
                    Err(CaptureError::PortalPending) if !portal_requested.get() => {
                        portal_requested.set(true);
                        controller.toast(&CaptureError::PortalPending.message());
                        portal_rx
                            .borrow_mut()
                            .replace(request_portal_source(&props.source));
                    }
                    Err(err) => {
                        if !warned.get() {
                            warned.set(true);
                            controller.toast(&err.message());
                        }
                    }
                }
            }

            let events = hub.borrow_mut().poll();
            for event in events {
                if let MediaEvent::Error { message, .. } = event {
                    controller.toast(&message);
                }
            }
            if let Some(view) = view.upgrade() {
                let count = ticks.get().wrapping_add(1);
                ticks.set(count);
                if count % 5 == 0 {
                    view.set_render_data(controller.render_data());
                }
            }
            glib::ControlFlow::Continue
        }
    });

    window.connect_close_request({
        let alive = alive.clone();
        let hub = hub.clone();
        move |_| {
            alive.set(false);
            hub.borrow_mut().stop_all();
            glib::Propagation::Proceed
        }
    });

    window.present();
}
