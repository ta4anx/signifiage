//! GTK UI: application, window, pages and rendering widgets.

use gtk4 as gtk;

pub mod app;
pub mod canvas;
pub mod design;
pub mod dialogs;
pub mod inspector;
pub mod outputs;
pub mod prompter_page;
pub mod prompter_view;
pub mod queue_page;
pub mod render;
pub mod run;
pub mod scene_view;
pub mod settings;
pub mod soundboard_page;
pub mod style;
pub mod widgets;
pub mod window;

pub use app::Startup;
pub use render::{RenderData, SceneView, ViewMode};

/// A top-level page inside the window's view stack.
pub trait Page {
    /// The widget added to the stack.
    fn widget(&self) -> gtk::Widget;
    /// Re-read controller/project state and update labels, lists and views.
    fn refresh(&self);
}

/// Name of the page for programmatic navigation (`design`, `run`, `queue`,
/// `prompter`, `soundboard`).
pub trait NamedPage: Page {
    fn name(&self) -> &'static str;
}
