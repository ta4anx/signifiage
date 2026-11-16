//! Embedded CSS and theme application.

use libadwaita as adw;

use crate::config::Theme;

const CSS: &str = include_str!("../../data/style.css");

/// Install the application stylesheet on the default display.
pub fn load() {
    let Some(display) = gtk4::gdk::Display::default() else {
        tracing::warn!("no display; stylesheet not installed");
        return;
    };
    let provider = gtk4::CssProvider::new();
    provider.load_from_string(CSS);
    gtk4::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

/// Force dark/light or follow the desktop; the accent colour is preserved.
pub fn apply_theme(theme: Theme) {
    let manager = adw::StyleManager::default();
    manager.set_color_scheme(match theme {
        Theme::Dark => adw::ColorScheme::ForceDark,
        Theme::Light => adw::ColorScheme::ForceLight,
        Theme::System => adw::ColorScheme::Default,
    });
}
