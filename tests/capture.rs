//! Capture planning: source/backend resolution never panics and never needs a
//! display or a live portal session.

use signifiage::config::CaptureBackend;
use signifiage::model::layer::CaptureSource;
use signifiage::services::capture::{CaptureError, CapturePlan, plan_for, plan_with_portal};
use signifiage::services::portal::PortalStream;

fn camera(device: &str) -> CaptureSource {
    CaptureSource::Camera {
        device: device.to_string(),
        label: "Camera".to_string(),
        width: Some(1280),
        height: Some(720),
        fps: Some(30),
    }
}

fn x11_window() -> CaptureSource {
    CaptureSource::X11Window {
        title: "Slides".to_string(),
        wm_class: "google-chrome".to_string(),
        xid: 0x1234,
    }
}

fn portal_screen(label: &str) -> CaptureSource {
    CaptureSource::Portal {
        label: label.to_string(),
        restore_token: Some("restore-token".to_string()),
    }
}

fn stream() -> PortalStream {
    PortalStream {
        fd: 7,
        node: 42,
        label: "Screen 1".to_string(),
        restore_token: Some("restore-token".to_string()),
    }
}

#[test]
fn camera_plans_a_v4l2_pipeline() {
    let plan = plan_with_portal(
        &camera("/dev/video0"),
        CaptureBackend::Auto,
        "appsink",
        false,
        None,
    )
    .expect("camera plan");
    assert!(matches!(plan, CapturePlan::Camera { .. }));
    let description = plan.description("appsink");
    assert!(
        description.contains("v4l2src device=/dev/video0"),
        "{description}"
    );
    assert!(description.contains("width=1280"), "{description}");
    assert!(description.contains("height=720"), "{description}");
    assert!(description.contains("framerate=30/1"), "{description}");
    assert!(description.contains("appsink"), "{description}");
}

#[test]
fn camera_without_a_device_is_an_error() {
    let err = plan_with_portal(&camera(""), CaptureBackend::Auto, "appsink", true, None)
        .expect_err("empty camera must fail");
    assert_eq!(err, CaptureError::NoDevice);
    assert!(!err.message().is_empty());
    assert!(!err.to_string().is_empty());
}

#[test]
fn x11_window_plans_an_ximagesrc_pipeline() {
    let plan = plan_with_portal(&x11_window(), CaptureBackend::X11, "appsink", true, None)
        .expect("x11 plan");
    assert!(matches!(plan, CapturePlan::X11 { .. }));
    let description = plan.description("appsink");
    assert!(description.contains("ximagesrc"), "{description}");
    assert!(description.contains("xid=4660"), "{description}");
    assert!(description.contains("show-pointer=false"), "{description}");
    assert!(description.contains("appsink"), "{description}");
}

#[test]
fn x11_window_without_a_display_reports_an_error() {
    let err = plan_with_portal(&x11_window(), CaptureBackend::X11, "appsink", false, None)
        .expect_err("x11 needs a display");
    assert_eq!(err, CaptureError::NoDisplay);
    assert!(!err.message().is_empty());
}

#[test]
fn auto_picks_x11_with_a_display_and_the_portal_without_one() {
    let with_display = plan_with_portal(&x11_window(), CaptureBackend::Auto, "appsink", true, None)
        .expect("x11 plan");
    assert!(matches!(with_display, CapturePlan::X11 { .. }));

    let without_display =
        plan_with_portal(&x11_window(), CaptureBackend::Auto, "appsink", false, None);
    assert_eq!(without_display, Err(CaptureError::PortalPending));
}

#[test]
fn portal_source_plans_a_pipewire_pipeline_from_the_approved_stream() {
    let plan = plan_with_portal(
        &portal_screen(""),
        CaptureBackend::Portal,
        "appsink",
        false,
        Some(stream()),
    )
    .expect("portal plan");
    match &plan {
        CapturePlan::Portal { fd, node, label } => {
            assert_eq!(*fd, 7);
            assert_eq!(*node, 42);
            // An unset source label falls back to the approved stream label.
            assert_eq!(label, "Screen 1");
        }
        other => panic!("expected a portal plan, got {other:?}"),
    }
    let description = plan.description("appsink");
    assert!(description.contains("pipewiresrc"), "{description}");
    assert!(description.contains("fd=7"), "{description}");
    assert!(description.contains("path=42"), "{description}");
    assert!(description.contains("appsink"), "{description}");
}

#[test]
fn portal_source_keeps_its_own_label() {
    let plan = plan_with_portal(
        &portal_screen("Studio screen"),
        CaptureBackend::Portal,
        "appsink",
        false,
        Some(stream()),
    )
    .expect("portal plan");
    assert_eq!(plan.label(), "Studio screen");
}

#[test]
fn portal_source_is_pending_until_it_is_approved() {
    let pending = plan_with_portal(
        &portal_screen("Screen 2"),
        CaptureBackend::Portal,
        "appsink",
        true,
        None,
    );
    assert_eq!(pending, Err(CaptureError::PortalPending));

    // The production entry point must behave the same way: no session, no panic.
    let pending = plan_for(&portal_screen("Screen 3"), CaptureBackend::Auto, "appsink");
    assert_eq!(pending, Err(CaptureError::PortalPending));
}

#[test]
fn x11_window_can_use_the_portal_backend() {
    let plan = plan_with_portal(
        &x11_window(),
        CaptureBackend::Portal,
        "appsink",
        true,
        Some(stream()),
    )
    .expect("portal plan for an x11 window");
    assert!(matches!(plan, CapturePlan::Portal { .. }));
}

#[test]
fn portal_source_rejects_the_x11_backend() {
    let err = plan_with_portal(
        &portal_screen("Screen 1"),
        CaptureBackend::X11,
        "appsink",
        true,
        Some(stream()),
    )
    .expect_err("portal source cannot be served by ximagesrc");
    assert!(matches!(err, CaptureError::Unsupported(_)));
    assert!(!err.message().is_empty());
}

#[test]
fn camera_ignores_the_backend_choice() {
    for backend in [
        CaptureBackend::Auto,
        CaptureBackend::X11,
        CaptureBackend::Portal,
    ] {
        let plan = plan_with_portal(&camera("/dev/video1"), backend, "appsink", false, None)
            .expect("camera is backend independent");
        assert!(matches!(plan, CapturePlan::Camera { .. }), "{backend:?}");
    }
}
