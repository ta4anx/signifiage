//! Small GTK helpers shared by every page: labelled rows, colour and font
//! buttons, dropdowns, toasts and section boxes.

use gtk::gdk;
use gtk::gio;
use gtk::pango;
use gtk::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;
use rust_i18n::t;

/// Box with a bold caption and its children, used in inspectors and dialogs.
pub fn section(title: &str) -> (gtk::Box, gtk::Box) {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 6);
    outer.add_css_class("section");
    let heading = gtk::Label::new(Some(title));
    heading.set_xalign(0.0);
    heading.add_css_class("heading");
    outer.append(&heading);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    outer.append(&content);
    (outer, content)
}

/// Horizontal row: caption on the left, control on the right.
pub fn labelled(caption: &str, child: &impl IsA<gtk::Widget>) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let label = gtk::Label::new(Some(caption));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label.set_valign(gtk::Align::Center);
    row.append(&label);
    row.append(child);
    row
}

/// Inline caption right next to a control (does not stretch).
pub fn inline(caption: &str, child: &impl IsA<gtk::Widget>) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let label = gtk::Label::new(Some(caption));
    label.set_valign(gtk::Align::Center);
    row.append(&label);
    row.append(child);
    row
}

/// Vertical field: caption above the control.
pub fn field(caption: &str, child: &impl IsA<gtk::Widget>) -> gtk::Box {
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let label = gtk::Label::new(Some(caption));
    label.set_xalign(0.0);
    label.add_css_class("caption");
    box_.append(&label);
    box_.append(child);
    box_
}

pub fn entry(text: &str) -> gtk::Entry {
    let entry = gtk::Entry::new();
    entry.set_text(text);
    entry.set_hexpand(true);
    entry
}

pub fn spin(min: f64, max: f64, step: f64, value: f64) -> gtk::SpinButton {
    let spin = gtk::SpinButton::with_range(min, max, step);
    spin.set_value(value);
    spin.set_numeric(true);
    spin.set_width_chars(7);
    spin
}

pub fn int_spin(min: i32, max: i32, value: i32) -> gtk::SpinButton {
    spin(f64::from(min), f64::from(max), 1.0, f64::from(value))
}

pub fn switch(active: bool) -> gtk::Switch {
    let switch = gtk::Switch::new();
    switch.set_active(active);
    switch.set_valign(gtk::Align::Center);
    switch
}

/// Dropdown over display strings; returns the widget and exposes the index.
pub fn dropdown(items: &[String], selected: usize) -> gtk::DropDown {
    let strings: Vec<&str> = items.iter().map(|item| item.as_str()).collect();
    let dropdown = gtk::DropDown::from_strings(&strings);
    if !items.is_empty() {
        dropdown.set_selected(selected.min(items.len() - 1) as u32);
    }
    dropdown
}

pub fn selected(dropdown: &gtk::DropDown) -> usize {
    dropdown.selected() as usize
}

/// Colour button that reports the chosen `#RRGGBBAA`.
pub fn color_button(color: &str) -> gtk::ColorDialogButton {
    let rgba = crate::ui::scene_view::gdk_rgba_str(color);
    let dialog = gtk::ColorDialog::builder().with_alpha(true).build();
    let button = gtk::ColorDialogButton::new(Some(dialog));
    button.set_rgba(&rgba);
    button.set_valign(gtk::Align::Center);
    button
}

/// `#RRGGBB`/`#RRGGBBAA` from a colour button.
pub fn color_hex(button: &gtk::ColorDialogButton) -> String {
    let rgba = button.rgba();
    let r = (rgba.red() * 255.0).round() as u8;
    let g = (rgba.green() * 255.0).round() as u8;
    let b = (rgba.blue() * 255.0).round() as u8;
    let a = (rgba.alpha() * 255.0).round() as u8;
    if a == 255 {
        format!("#{r:02X}{g:02X}{b:02X}")
    } else {
        format!("#{r:02X}{g:02X}{b:02X}{a:02X}")
    }
}

pub fn font_button(family: &str, size: f64) -> gtk::FontDialogButton {
    let dialog = gtk::FontDialog::builder().build();
    let button = gtk::FontDialogButton::new(Some(dialog));
    button.set_use_font(true);
    button.set_use_size(true);
    button.set_valign(gtk::Align::Center);
    set_font(&button, family, size);
    button
}

pub fn set_font(button: &gtk::FontDialogButton, family: &str, size: f64) {
    let description = pango::FontDescription::new();
    let mut description = description;
    description.set_family(family);
    description.set_absolute_size(size * f64::from(pango::SCALE));
    button.set_font_desc(&description);
}

pub fn font_family(button: &gtk::FontDialogButton) -> String {
    button
        .font_desc()
        .and_then(|description| description.family().map(|family| family.to_string()))
        .unwrap_or_else(|| "Sans".to_string())
}

pub fn font_size(button: &gtk::FontDialogButton) -> f64 {
    button
        .font_desc()
        .map(|description| description.size() as f64 / f64::from(pango::SCALE))
        .filter(|size| *size > 0.0)
        .unwrap_or(64.0)
}

/// Toast through the window overlay.
pub fn toast(overlay: &adw::ToastOverlay, message: &str) {
    overlay.add_toast(adw::Toast::new(message));
}

/// Simple modal message dialog.
pub fn alert(parent: Option<&gtk::Window>, title: &str, body: &str) {
    let dialog = gtk::AlertDialog::builder()
        .message(title)
        .detail(body)
        .modal(true)
        .build();
    dialog.show(parent);
}

/// Yes/no confirmation.
pub fn confirm(
    parent: Option<&gtk::Window>,
    title: &str,
    body: &str,
    on_confirm: impl Fn() + 'static,
) {
    let dialog = gtk::AlertDialog::builder()
        .message(title)
        .detail(body)
        .modal(true)
        .build();
    dialog.set_buttons(&[&t!("common.cancel"), &t!("common.confirm")]);
    dialog.set_cancel_button(0);
    dialog.set_default_button(1);
    let parent = parent.cloned();
    dialog.choose(parent.as_ref(), gio::Cancellable::NONE, move |result| {
        if let Ok(index) = result
            && index == 1
        {
            on_confirm();
        }
    });
}

pub fn gdk_color(value: &str) -> gdk::RGBA {
    crate::ui::scene_view::gdk_rgba_str(value)
}

pub fn separator() -> gtk::Separator {
    gtk::Separator::new(gtk::Orientation::Horizontal)
}

/// Toolbar-style flat button with an icon and tooltip.
pub fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.set_tooltip_text(Some(tooltip));
    button.add_css_class("flat");
    button
}

/// Compact button with a label.
pub fn button(label: &str) -> gtk::Button {
    gtk::Button::with_label(label)
}

/// Destructive button styling.
pub fn destructive(label: &str) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    button.add_css_class("destructive-action");
    button
}

pub fn suggested(label: &str) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    button.add_css_class("suggested-action");
    button
}

pub fn dim_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.add_css_class("dim-label");
    label
}

pub fn wrap_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_wrap(true);
    label.set_xalign(0.0);
    label
}
