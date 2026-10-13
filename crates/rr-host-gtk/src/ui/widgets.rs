//! Small builders the pages share: accessible names, rows, QR pictures, error labels.

use adw::prelude::*;
use gtk::{gdk, glib};

/// Sets what assistive technologies (and the AT-SPI UI test) call `w`.
/// (A button's own label names it through "labelled by", which wins over a name given: that goes,
/// so call this again after changing the button's label.)
pub fn name(w: &impl IsA<gtk::Accessible>, label: &str) {
    w.reset_relation(gtk::AccessibleRelation::LabelledBy);
    w.update_property(&[gtk::accessible::Property::Label(label)]);
}

pub fn button(label: &str) -> gtk::Button {
    let b = gtk::Button::with_label(label);
    b.set_valign(gtk::Align::Center);
    b
}

/// A button with an icon only, named for assistive technologies.
pub fn icon_button(icon: &str, label: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.set_valign(gtk::Align::Center);
    b.set_tooltip_text(Some(label));
    b.add_css_class("flat");
    name(&b, label);
    b
}

pub fn row(title: &str, subtitle: &str) -> adw::ActionRow {
    let r = adw::ActionRow::new();
    r.set_title(&glib::markup_escape_text(title));
    if !subtitle.is_empty() {
        r.set_subtitle(&glib::markup_escape_text(subtitle));
    }
    r
}

pub fn group(title: &str, description: &str) -> adw::PreferencesGroup {
    let g = adw::PreferencesGroup::new();
    g.set_title(title);
    if !description.is_empty() {
        g.set_description(Some(description));
    }
    g
}

/// A switch in a row, named after the row.
pub fn switch_row(title: &str, subtitle: &str) -> (adw::ActionRow, gtk::Switch) {
    let r = row(title, subtitle);
    let s = gtk::Switch::new();
    s.set_valign(gtk::Align::Center);
    name(&s, title);
    r.add_suffix(&s);
    r.set_activatable_widget(Some(&s));
    (r, s)
}

/// An entry in a row, named after the row.
pub fn entry_row(title: &str, subtitle: &str, placeholder: &str) -> (adw::ActionRow, gtk::Entry) {
    let r = row(title, subtitle);
    let e = gtk::Entry::new();
    e.set_valign(gtk::Align::Center);
    e.set_placeholder_text(Some(placeholder));
    e.set_width_chars(22);
    name(&e, title);
    r.add_suffix(&e);
    (r, e)
}

/// One choice of several, as a row of linked toggle buttons (a segmented control; unlike check
/// buttons, GTK gives toggle buttons an action assistive technologies can do).
pub fn segmented(title: &str, subtitle: &str, choices: &[(&'static str, &str)]) -> (adw::ActionRow, Vec<(&'static str, gtk::ToggleButton)>) {
    let r = row(title, subtitle);
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    b.add_css_class("linked");
    b.set_valign(gtk::Align::Center);
    let mut out: Vec<(&'static str, gtk::ToggleButton)> = Vec::new();
    for (id, label) in choices {
        let t = gtk::ToggleButton::with_label(label);
        if let Some((_, first)) = out.first() {
            t.set_group(Some(first));
        }
        b.append(&t);
        out.push((id, t));
    }
    r.add_suffix(&b);
    (r, out)
}

/// The id of the choice that's on.
pub fn chosen(choices: &[(&'static str, gtk::ToggleButton)]) -> Option<&'static str> {
    choices.iter().find(|(_, t)| t.is_active()).map(|(id, _)| *id)
}

/// Turns on the choice `id` (or the first, when none is `id`).
pub fn choose(choices: &[(&'static str, gtk::ToggleButton)], id: &str) {
    if let Some((_, t)) = choices.iter().find(|(c, _)| *c == id).or(choices.first()) {
        t.set_active(true);
    }
}

/// A label for errors: red, wrapping, selectable, hidden while empty.
pub fn error_label() -> gtk::Label {
    let l = gtk::Label::new(None);
    l.add_css_class("error");
    l.set_wrap(true);
    l.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    l.set_selectable(true);
    l.set_xalign(0.0);
    l.set_visible(false);
    l.set_margin_top(6);
    l
}

/// A label for what happened (a save's result), dimmed, hidden while empty.
pub fn note_label() -> gtk::Label {
    let l = gtk::Label::new(None);
    l.add_css_class("dim-label");
    l.set_wrap(true);
    l.set_xalign(0.0);
    l.set_visible(false);
    l.set_margin_top(6);
    l
}

pub fn set_text(l: &gtk::Label, text: &str) {
    l.set_text(text);
    l.set_visible(!text.is_empty());
}

/// `text` as a QR code texture.
pub fn qr_texture(text: &str) -> Option<gdk::Texture> {
    let img = rr_host_gtk::qr::render(text, 6)?;
    let bytes = glib::Bytes::from_owned(img.rgba);
    let t = gdk::MemoryTexture::new(img.size as i32, img.size as i32, gdk::MemoryFormat::R8g8b8a8, &bytes, img.size * 4);
    Some(t.upcast())
}

/// A picture for a QR code, a fixed size.
pub fn qr_picture(label: &str) -> gtk::Picture {
    let p = gtk::Picture::new();
    p.set_size_request(220, 220);
    p.set_can_shrink(true);
    p.set_content_fit(gtk::ContentFit::Contain);
    p.set_halign(gtk::Align::Center);
    p.set_margin_top(12);
    p.set_margin_bottom(6);
    p.set_alternative_text(Some(label));
    name(&p, label);
    p
}

/// Rows added to a group, kept to be taken out again.
#[derive(Default)]
pub struct Rows(std::cell::RefCell<Vec<gtk::Widget>>);

impl Rows {
    pub fn clear(&self, g: &adw::PreferencesGroup) {
        for w in self.0.borrow_mut().drain(..) {
            g.remove(&w);
        }
    }
    pub fn add(&self, g: &adw::PreferencesGroup, w: &impl IsA<gtk::Widget>) {
        g.add(w);
        self.0.borrow_mut().push(w.clone().upcast());
    }
}

/// Asks before something that can't be undone: `on_yes` after the person says `yes`.
pub fn confirm(parent: &impl IsA<gtk::Widget>, heading: &str, body: &str, yes: &str, extra: Option<&gtk::Widget>, on_yes: impl Fn() + 'static) {
    let d = adw::AlertDialog::new(Some(heading), Some(body));
    d.add_responses(&[("cancel", "Cancel"), ("yes", yes)]);
    d.set_response_appearance("yes", adw::ResponseAppearance::Destructive);
    d.set_default_response(Some("cancel"));
    d.set_close_response("cancel");
    if let Some(e) = extra {
        d.set_extra_child(Some(e));
    }
    d.connect_response(None, move |_, r| {
        if r == "yes" {
            on_yes();
        }
    });
    d.present(Some(parent));
}
