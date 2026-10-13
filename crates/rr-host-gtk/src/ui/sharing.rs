//! Sharing: on or off, what's shared, the links (each a key to the screen) with a QR code, and the
//! pairing code with its countdown.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use rr_host_gtk::model::{source_label, Link, Sources};
use rr_host_gtk::time;

use super::widgets::{self, Rows};
use super::{Change, Ctx};

struct Page {
    status: adw::ActionRow,
    start_stop: gtk::Button,
    restart: gtk::Button,
    error: gtk::Label,
    sources: adw::PreferencesGroup,
    source_rows: Rows,
    /// Each source's row: its id, the mark on the one shared, its Share button.
    source_marks: RefCell<Vec<(String, String, gtk::Image, gtk::Button)>>,
    source_error: gtk::Label,
    last_sources: RefCell<Sources>,
    links: adw::PreferencesGroup,
    link_rows: Rows,
    link_qr: gtk::Picture,
    link_qr_caption: gtk::Label,
    qr_for: RefCell<Option<String>>,
    code: gtk::Label,
    countdown: gtk::Label,
    new_code: gtk::Button,
    pair_qr: gtk::Picture,
    pair_note: gtk::Label,
    fingerprint: adw::ActionRow,
}

pub fn page(ctx: &Rc<Ctx>) -> adw::PreferencesPage {
    let pp = adw::PreferencesPage::new();

    // Sharing on or off.
    let g = widgets::group("Sharing", "");
    let status = widgets::row("Not sharing", "");
    let start_stop = widgets::button("Start sharing");
    start_stop.add_css_class("suggested-action");
    let restart = widgets::button("Restart sharing");
    restart.set_tooltip_text(Some("Settings changed while viewers were connected apply when sharing restarts"));
    restart.set_visible(false);
    status.add_suffix(&restart);
    status.add_suffix(&start_stop);
    g.add(&status);
    let error = widgets::error_label();
    g.add(&error);
    pp.add(&g);

    // What's shared.
    let sources = widgets::group("What's shared", "Switching is live: viewers see the new source without reconnecting.");
    let refresh = widgets::icon_button("view-refresh-symbolic", "Refresh the sources");
    sources.set_header_suffix(Some(&refresh));
    let source_error = widgets::error_label();
    pp.add(&sources);

    let (other_row, other_entry) = widgets::entry_row("Another source", "vnc:HOST:PORT, test-pattern:WxH, window:ID", "vnc:127.0.0.1:5900");
    let other_go = widgets::button("Share");
    widgets::name(&other_go, "Share this source");
    other_row.add_suffix(&other_go);
    let other_group = adw::PreferencesGroup::new();
    other_group.add(&other_row);
    other_group.add(&source_error);
    pp.add(&other_group);

    // The links.
    let links = widgets::group(
        "Links",
        "Each link is a key to this screen: whoever has it can watch and control. Share it only with who should.",
    );
    pp.add(&links);
    let qr_group = adw::PreferencesGroup::new();
    let link_qr = widgets::qr_picture("QR code of the link");
    let link_qr_caption = widgets::note_label();
    link_qr_caption.set_halign(gtk::Align::Center);
    link_qr_caption.set_xalign(0.5);
    qr_group.add(&link_qr);
    qr_group.add(&link_qr_caption);
    pp.add(&qr_group);

    // Pairing.
    let pg = widgets::group(
        "Pairing",
        "A phone or computer pairs once with this code (or by scanning the code below), then connects with its own key.",
    );
    let code_row = widgets::row("Pairing code", "");
    let code = gtk::Label::new(Some("–"));
    code.add_css_class("title-1");
    code.set_selectable(true);
    let countdown = gtk::Label::new(None);
    countdown.add_css_class("dim-label");
    countdown.add_css_class("numeric");
    let new_code = widgets::button("New code");
    code_row.add_suffix(&countdown);
    code_row.add_suffix(&code);
    code_row.add_suffix(&new_code);
    pg.add(&code_row);
    let pair_note = widgets::note_label();
    pg.add(&pair_note);
    let pair_qr = widgets::qr_picture("QR code for pairing");
    pg.add(&pair_qr);
    let fingerprint = widgets::row("This computer's fingerprint", "");
    fingerprint.set_subtitle_selectable(true);
    pg.add(&fingerprint);
    pp.add(&pg);

    let p = Rc::new(Page {
        status,
        start_stop: start_stop.clone(),
        restart: restart.clone(),
        error,
        sources,
        source_rows: Rows::default(),
        source_marks: RefCell::new(vec![]),
        source_error,
        last_sources: RefCell::new(Sources::default()),
        links,
        link_rows: Rows::default(),
        link_qr,
        link_qr_caption,
        qr_for: RefCell::new(None),
        code,
        countdown,
        new_code: new_code.clone(),
        pair_qr,
        pair_note,
        fingerprint,
    });

    {
        let ctx2 = ctx.clone();
        start_stop.connect_clicked(move |_| {
            let on = !ctx2.state.borrow().sharing;
            ctx2.set_sharing(on);
        });
    }
    {
        let ctx2 = ctx.clone();
        restart.connect_clicked(move |_| {
            ctx2.call(
                |c| c.restart(),
                |ctx, r| match r {
                    Ok(s) => ctx.set_state(s),
                    Err(e) => ctx.toast(&e.message),
                },
            );
        });
    }
    {
        let ctx2 = ctx.clone();
        new_code.connect_clicked(move |_| ctx2.new_code());
    }
    {
        let (ctx2, p2) = (ctx.clone(), p.clone());
        refresh.connect_clicked(move |_| load_sources(&ctx2, &p2));
    }
    {
        let (ctx2, p2, e2) = (ctx.clone(), p.clone(), other_entry.clone());
        let go = move || {
            let id = e2.text().trim().to_owned();
            if !id.is_empty() {
                switch_source(&ctx2, &p2, id);
            }
        };
        let go = Rc::new(go);
        let g2 = go.clone();
        other_go.connect_clicked(move |_| g2());
        other_entry.connect_activate(move |_| go());
    }

    {
        let (ctx2, p2) = (ctx.clone(), p.clone());
        ctx.listen(move |c| match c {
            Change::Link => {
                if ctx2.connected() {
                    load_sources(&ctx2, &p2);
                    load_fingerprint(&ctx2, &p2);
                }
                draw(&ctx2, &p2);
            }
            Change::State => draw(&ctx2, &p2),
            Change::Page if ctx2.page() == "sharing" && ctx2.connected() => load_sources(&ctx2, &p2),
            _ => {}
        });
    }
    // The countdown, from the code's expiry.
    {
        let (ctx2, p2) = (ctx.clone(), p.clone());
        glib::timeout_add_seconds_local(1, move || {
            draw_countdown(&ctx2, &p2);
            glib::ControlFlow::Continue
        });
    }
    draw(ctx, &p);
    pp
}

fn load_sources(ctx: &Rc<Ctx>, p: &Rc<Page>) {
    let p = p.clone();
    ctx.call(
        |c| c.sources(),
        move |ctx, r| match r {
            Ok(s) => {
                *p.last_sources.borrow_mut() = s;
                draw_sources(ctx, &p);
            }
            Err(e) => widgets::set_text(&p.source_error, &e.message),
        },
    );
}

fn load_fingerprint(ctx: &Rc<Ctx>, p: &Rc<Page>) {
    let p = p.clone();
    ctx.call(
        |c| c.pairing(),
        move |_, r| {
            if let Ok(pairing) = r {
                p.fingerprint.set_subtitle(pairing.fingerprint.as_deref().unwrap_or("–"));
            }
        },
    );
}

fn switch_source(ctx: &Rc<Ctx>, p: &Rc<Page>, id: String) {
    let p = p.clone();
    widgets::set_text(&p.source_error, "");
    ctx.call(
        move |c| c.set_source(&id),
        move |ctx, r| match r {
            Ok(s) => {
                ctx.set_state(s);
                ctx.toast(&format!("Now sharing: {}", source_label(&ctx.state.borrow().source)));
            }
            Err(e) => {
                widgets::set_text(&p.source_error, &e.message);
                ctx.toast(&e.message);
                draw_sources(ctx, &p);
            }
        },
    );
}

/// The sources as radio buttons, the current one chosen.
fn draw_sources(ctx: &Rc<Ctx>, p: &Rc<Page>) {
    p.source_rows.clear(&p.sources);
    p.source_marks.borrow_mut().clear();
    let s = p.last_sources.borrow().clone();
    let current = ctx.state.borrow().source.clone();
    let mut entries: Vec<(String, String, String)> = Vec::new();
    for d in &s.displays {
        let size = if d.width > 0 { format!("{}×{}", d.width, d.height) } else { String::new() };
        entries.push((d.id.clone(), d.name.clone(), size));
    }
    for a in &s.apps {
        entries.push((a.id.clone(), a.name.clone(), format!("all its windows ({})", a.windows)));
    }
    for o in &s.other {
        entries.push((o.id.clone(), o.name.clone(), String::new()));
    }
    let known = entries.iter().any(|e| e.0 == current) || s.windows.iter().any(|w| w.id == current);
    if !known && !current.is_empty() {
        entries.insert(0, (current.clone(), source_label(&current), "shared now".into()));
    }
    let add = |id: &str, title: &str, sub: &str, parent: Option<&adw::ExpanderRow>| {
        let row = widgets::row(title, sub);
        let mark = gtk::Image::from_icon_name("object-select-symbolic");
        mark.set_tooltip_text(Some("Shared now"));
        row.add_prefix(&mark);
        let b = widgets::button("Share");
        widgets::name(&b, &format!("Share {title}"));
        let (ctx2, p2, id2) = (ctx.clone(), p.clone(), id.to_owned());
        b.connect_clicked(move |_| switch_source(&ctx2, &p2, id2.clone()));
        row.add_suffix(&b);
        p.source_marks.borrow_mut().push((id.to_owned(), title.to_owned(), mark, b));
        match parent {
            Some(e) => e.add_row(&row),
            None => p.source_rows.add(&p.sources, &row),
        }
    };
    for (id, title, sub) in &entries {
        add(id, title, sub, None);
    }
    if !s.windows.is_empty() {
        let exp = adw::ExpanderRow::new();
        exp.set_title(&format!("Windows ({})", s.windows.len()));
        exp.set_subtitle("One window: it shows as it is even when covered");
        exp.set_expanded(s.windows.iter().any(|w| w.id == current));
        for w in &s.windows {
            let title = if w.title.is_empty() { w.app.clone() } else { w.title.clone() };
            let sub = format!("{} · {}×{}", if w.app.is_empty() { &w.id } else { &w.app }, w.width, w.height);
            add(&w.id, &title, &sub, Some(&exp));
        }
        p.source_rows.add(&p.sources, &exp);
    }
    mark_source(&current, p);
    widgets::set_text(&p.source_error, s.error.as_deref().unwrap_or(""));
}

/// The mark on the source shared; its button says so.
fn mark_source(current: &str, p: &Page) {
    for (id, title, mark, b) in p.source_marks.borrow().iter() {
        let on = id == current;
        mark.set_opacity(if on { 1.0 } else { 0.0 });
        b.set_label(if on { "Shared" } else { "Share" });
        // (A new label renames the button: named again.)
        widgets::name(b, &format!("Share {title}"));
        b.set_sensitive(!on);
    }
}

fn draw(ctx: &Rc<Ctx>, p: &Rc<Page>) {
    let connected = ctx.connected();
    let s = ctx.state.borrow().clone();
    p.status.set_title(&ctx.status_text());
    p.status.set_subtitle(&glib::markup_escape_text(&if connected {
        format!("{} · {}", source_label(&s.source), if s.listen.is_empty() { "–" } else { &s.listen })
    } else {
        String::new()
    }));
    p.start_stop.set_sensitive(connected);
    p.start_stop.set_label(if s.sharing { "Stop sharing" } else { "Start sharing" });
    if s.sharing {
        p.start_stop.remove_css_class("suggested-action");
        p.start_stop.add_css_class("destructive-action");
    } else {
        p.start_stop.remove_css_class("destructive-action");
        p.start_stop.add_css_class("suggested-action");
    }
    p.restart.set_visible(connected && s.sharing && s.restart_needed);
    widgets::set_text(&p.error, if connected { s.error.as_deref().unwrap_or("") } else { "" });

    // The source shared follows the state.
    if !p.source_marks.borrow().iter().any(|(id, _, _, _)| *id == s.source) && connected && !p.last_sources.borrow().displays.is_empty() {
        draw_sources(ctx, p);
    }
    mark_source(&s.source, p);

    draw_links(ctx, p, &s.links);

    // Pairing.
    let pairing = &s.pairing;
    p.code.set_text(pairing.code.as_deref().unwrap_or("–"));
    p.new_code.set_sensitive(connected && s.sharing && pairing.enabled);
    let note = if !connected {
        ""
    } else if !s.sharing {
        "Pairing needs sharing on."
    } else if !pairing.enabled {
        "Pairing is off (Settings, Pairing)."
    } else if pairing.code.is_none() {
        "No open code (used, expired, or too many wrong tries): make a new one."
    } else {
        "The code lasts 10 minutes and pairs one device."
    };
    widgets::set_text(&p.pair_note, note);
    match pairing.qr.as_deref().filter(|_| pairing.code.is_some()) {
        Some(q) => {
            p.pair_qr.set_paintable(widgets::qr_texture(q).as_ref());
            p.pair_qr.set_visible(true);
        }
        None => p.pair_qr.set_visible(false),
    }
    draw_countdown(ctx, p);
}

fn draw_links(ctx: &Rc<Ctx>, p: &Rc<Page>, links: &[Link]) {
    p.link_rows.clear(&p.links);
    if links.is_empty() {
        let r = widgets::row(
            if ctx.connected() {
                "No links while not sharing"
            } else {
                "Not connected to the host"
            },
            "",
        );
        p.link_rows.add(&p.links, &r);
        p.link_qr.set_visible(false);
        widgets::set_text(&p.link_qr_caption, "");
        return;
    }
    // The QR code: the link chosen, else the best one.
    let chosen = p.qr_for.borrow().clone();
    let shown = links.iter().find(|l| Some(&l.url) == chosen.as_ref()).unwrap_or(&links[0]).clone();
    for l in links {
        let r = widgets::row(&l.label, &l.url);
        r.set_subtitle_selectable(true);
        r.set_subtitle_lines(2);
        let copy = widgets::icon_button("edit-copy-symbolic", &format!("Copy the link for {}", l.label));
        let url = l.url.clone();
        let ctx2 = ctx.clone();
        copy.connect_clicked(move |b| {
            b.clipboard().set_text(&url);
            ctx2.toast("Link copied. It's a key to this screen.");
        });
        let qr = widgets::icon_button("view-grid-symbolic", &format!("Show the QR code for {}", l.label));
        let (p2, l2, ctx2) = (p.clone(), l.clone(), ctx.clone());
        qr.connect_clicked(move |_| {
            *p2.qr_for.borrow_mut() = Some(l2.url.clone());
            let links = ctx2.state.borrow().links.clone();
            draw_links(&ctx2, &p2, &links);
        });
        r.add_suffix(&qr);
        r.add_suffix(&copy);
        p.link_rows.add(&p.links, &r);
    }
    p.link_qr.set_paintable(widgets::qr_texture(&shown.qr).as_ref());
    p.link_qr.set_visible(true);
    widgets::set_text(&p.link_qr_caption, &format!("Scan to open: {}", shown.label));
}

fn draw_countdown(ctx: &Rc<Ctx>, p: &Rc<Page>) {
    let s = ctx.state.borrow();
    let left = match (&s.pairing.code, &s.pairing.expires_at) {
        (Some(_), Some(at)) if ctx.connected() => time::seconds_left(at, time::now()),
        _ => None,
    };
    match left {
        Some(0) => p.countdown.set_text("expired"),
        Some(n) => p.countdown.set_text(&format!("{} left", time::countdown(n))),
        None => p.countdown.set_text(""),
    }
}
