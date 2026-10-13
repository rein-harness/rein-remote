//! The admin pages: sign-in (OIDC, SAML; secrets write-only), the policy (checked by the host's own
//! parser, its message shown), live sessions, the audit log (filters, pages), metrics.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use rr_host_gtk::model::{applied_text, value_text, AuditQuery, Signin, AUDIT_EVENTS};
use rr_host_gtk::time;
use serde_json::{Map, Value};

use super::widgets::{self, Rows};
use super::{viewers, Change, Ctx};

// Sign-in.

const OIDC: &[(&str, &str, &str)] = &[
    ("public-url", "Public URL", "https://remote.example.com: where viewers reach this host"),
    ("oidc-issuer", "Issuer", "https://example.okta.com"),
    ("oidc-client-id", "Client ID", ""),
    ("oidc-scopes", "Scopes", "openid email profile"),
    ("oidc-user-claim", "User claim", "email"),
    ("oidc-groups-claim", "Groups claim", "groups"),
];

const SAML: &[(&str, &str, &str)] = &[
    ("saml-idp-metadata", "IdP metadata", "a file or https:// URL"),
    ("saml-entity-id", "Entity ID", ""),
    ("saml-user-attr", "User attribute", ""),
    ("saml-groups-attr", "Groups attribute", "groups"),
];

pub fn signin_page(ctx: &Rc<Ctx>) -> adw::PreferencesPage {
    let pp = adw::PreferencesPage::new();
    let kind = widgets::group("Sign-in", "Viewers sign in with your identity provider before they see the screen.");
    let (r2, kinds) = widgets::segmented(
        "Sign in with",
        "OpenID Connect: Okta, Entra ID, Google, Keycloak…",
        &[("off", "Off"), ("oidc", "OpenID Connect"), ("saml", "SAML")],
    );
    kind.add(&r2);
    let (off, oidc, saml) = (kinds[0].1.clone(), kinds[1].1.clone(), kinds[2].1.clone());
    pp.add(&kind);

    let og = widgets::group("OpenID Connect", "");
    let mut entries: Vec<(&'static str, gtk::Entry)> = Vec::new();
    for (k, title, hint) in OIDC {
        let (r, e) = widgets::entry_row(title, "", hint);
        og.add(&r);
        entries.push((k, e));
    }
    let secret_row = widgets::row("Client secret", "");
    let secret = gtk::PasswordEntry::new();
    secret.set_show_peek_icon(true);
    secret.set_valign(gtk::Align::Center);
    secret.set_width_chars(22);
    widgets::name(&secret, "Client secret");
    secret_row.add_suffix(&secret);
    og.add(&secret_row);
    pp.add(&og);

    let sg = widgets::group("SAML", "");
    for (k, title, hint) in SAML {
        let (r, e) = widgets::entry_row(title, "", hint);
        sg.add(&r);
        entries.push((k, e));
    }
    pp.add(&sg);

    let bottom = adw::PreferencesGroup::new();
    let save = widgets::button("Save sign-in");
    save.add_css_class("suggested-action");
    save.set_halign(gtk::Align::End);
    let error = widgets::error_label();
    let note = widgets::note_label();
    bottom.add(&save);
    bottom.add(&error);
    bottom.add(&note);
    pp.add(&bottom);

    let entries = Rc::new(entries);
    let loaded: Rc<RefCell<Signin>> = Rc::default();
    let show_groups = {
        let (og, sg, oidc, saml) = (og.clone(), sg.clone(), oidc.clone(), saml.clone());
        move || {
            og.set_visible(oidc.is_active());
            sg.set_visible(saml.is_active());
        }
    };
    let show_groups = Rc::new(show_groups);
    for c in [&off, &oidc, &saml] {
        let sg = show_groups.clone();
        c.connect_toggled(move |_| sg());
    }
    let fill = {
        let (entries, loaded, secret_row, secret, off, oidc, saml, r2, sg) = (
            entries.clone(),
            loaded.clone(),
            secret_row.clone(),
            secret.clone(),
            off.clone(),
            oidc.clone(),
            saml.clone(),
            r2.clone(),
            show_groups.clone(),
        );
        Rc::new(move |s: Signin| {
            for (k, e) in entries.iter() {
                e.set_text(&s.str(k));
            }
            secret.set_text("");
            secret_row.set_subtitle(if s.bool("oidc-client-secret-set") {
                "Set (never shown). Type a new one to replace it."
            } else {
                "None set."
            });
            match s.kind().as_deref() {
                Some("oidc") => oidc.set_active(true),
                Some("saml") => saml.set_active(true),
                _ => off.set_active(true),
            }
            let available = s.bool("saml_available");
            saml.set_sensitive(available);
            r2.set_subtitle(if available {
                "OpenID Connect: Okta, Entra ID, Google, Keycloak…"
            } else {
                "OpenID Connect: Okta, Entra ID, Google, Keycloak…. SAML isn't in this build (built without --features saml)."
            });
            *loaded.borrow_mut() = s;
            sg();
        })
    };
    let load = {
        let (ctx, fill, error) = (ctx.clone(), fill.clone(), error.clone());
        move || {
            let (fill, error) = (fill.clone(), error.clone());
            ctx.call(
                |c| c.signin(),
                move |_, r| match r {
                    Ok(s) => fill(s),
                    Err(e) => widgets::set_text(&error, &e.message),
                },
            );
        }
    };
    {
        let (ctx2, entries, secret, oidc, saml, error, note, fill) = (
            ctx.clone(),
            entries.clone(),
            secret.clone(),
            oidc.clone(),
            saml.clone(),
            error.clone(),
            note.clone(),
            fill.clone(),
        );
        save.connect_clicked(move |_| {
            let mut m = Map::new();
            let text = |k: &str| -> Value {
                let t = entries
                    .iter()
                    .find(|(e, _)| *e == k)
                    .map(|(_, e)| e.text().trim().to_owned())
                    .unwrap_or_default();
                if t.is_empty() {
                    Value::Null
                } else {
                    Value::from(t)
                }
            };
            if oidc.is_active() {
                for (k, _, _) in OIDC {
                    m.insert((*k).into(), text(k));
                }
                m.insert("saml-idp-metadata".into(), Value::Null);
                let s = secret.text().to_string();
                if !s.is_empty() {
                    m.insert("oidc-client-secret".into(), Value::from(s));
                }
            } else if saml.is_active() {
                m.insert("public-url".into(), text("public-url"));
                for (k, _, _) in SAML {
                    m.insert((*k).into(), text(k));
                }
                m.insert("oidc-issuer".into(), Value::Null);
            } else {
                m.insert("oidc-issuer".into(), Value::Null);
                m.insert("saml-idp-metadata".into(), Value::Null);
            }
            widgets::set_text(&error, "");
            let (error, note, fill) = (error.clone(), note.clone(), fill.clone());
            ctx2.call(
                move |c| c.put_signin(&m),
                move |_, r| match r {
                    Ok(s) => {
                        widgets::set_text(&note, applied_text(s.applied().as_deref()).unwrap_or("Saved."));
                        fill(s);
                    }
                    Err(e) => {
                        widgets::set_text(&error, &e.message);
                        widgets::set_text(&note, "");
                    }
                },
            );
        });
    }
    {
        let ctx2 = ctx.clone();
        ctx.listen(move |c| {
            if (c == Change::Link && ctx2.connected()) || (c == Change::Page && ctx2.page() == "signin" && ctx2.connected()) {
                load();
            }
        });
    }
    show_groups();
    pp
}

// Policy.

pub fn policy_page(ctx: &Rc<Ctx>) -> adw::PreferencesPage {
    let pp = adw::PreferencesPage::new();
    let g = widgets::group(
        "Policy",
        "Rules matched in order; the first that matches decides who may connect, and what they may do. The host checks the text with its own parser before using it.",
    );
    let status = widgets::row("No policy", "");
    status.set_subtitle_selectable(true);
    g.add(&status);
    let (fr, formats) = widgets::segmented("Format", "", &[("toml", "TOML"), ("json", "JSON")]);
    let (toml, json) = (formats[0].1.clone(), formats[1].1.clone());
    toml.set_active(true);
    g.add(&fr);
    pp.add(&g);

    let eg = adw::PreferencesGroup::new();
    let view = gtk::TextView::new();
    view.set_monospace(true);
    view.set_wrap_mode(gtk::WrapMode::WordChar);
    view.set_top_margin(8);
    view.set_bottom_margin(8);
    view.set_left_margin(8);
    view.set_right_margin(8);
    widgets::name(&view, "Policy text");
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_min_content_height(260);
    scroll.set_child(Some(&view));
    let frame = gtk::Frame::new(None);
    frame.set_child(Some(&scroll));
    eg.add(&frame);
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    bar.set_halign(gtk::Align::End);
    bar.set_margin_top(12);
    let off = widgets::button("Turn the policy off");
    let reload = widgets::button("Reload");
    widgets::name(&reload, "Reload the policy");
    let save = widgets::button("Check and save");
    save.add_css_class("suggested-action");
    widgets::name(&save, "Check and save the policy");
    bar.append(&off);
    bar.append(&reload);
    bar.append(&save);
    eg.add(&bar);
    let error = widgets::error_label();
    let note = widgets::note_label();
    eg.add(&error);
    eg.add(&note);
    pp.add(&eg);

    let fill = {
        let (status, view, toml, json) = (status.clone(), view.clone(), toml.clone(), json.clone());
        Rc::new(move |p: rr_host_gtk::model::Policy| {
            if p.enabled {
                status.set_title(&format!("{} rule{}", p.rules, if p.rules == 1 { "" } else { "s" }));
                status.set_subtitle(&glib::markup_escape_text(p.path.as_deref().unwrap_or("")));
            } else {
                status.set_title("No policy");
                status.set_subtitle("Everyone the link or pairing lets in may watch and control.");
            }
            if p.format.as_deref() == Some("json") {
                json.set_active(true);
            } else {
                toml.set_active(true);
            }
            view.buffer().set_text(p.text.as_deref().unwrap_or(""));
        })
    };
    let load = {
        let (ctx, fill, error) = (ctx.clone(), fill.clone(), error.clone());
        Rc::new(move || {
            let (fill, error) = (fill.clone(), error.clone());
            ctx.call(
                |c| c.policy(),
                move |_, r| match r {
                    Ok(p) => {
                        widgets::set_text(&error, "");
                        fill(p)
                    }
                    Err(e) => widgets::set_text(&error, &e.message),
                },
            );
        })
    };
    let put = {
        let (ctx, fill, error, note, json) = (ctx.clone(), fill.clone(), error.clone(), note.clone(), json.clone());
        Rc::new(move |text: Option<String>| {
            let format = if json.is_active() { "json" } else { "toml" };
            let (fill, error, note) = (fill.clone(), error.clone(), note.clone());
            widgets::set_text(&error, "");
            widgets::set_text(&note, "");
            ctx.call(
                move |c| c.put_policy(text.as_deref(), format),
                move |_, r| match r {
                    Ok(p) => {
                        widgets::set_text(&note, applied_text(p.applied.as_deref()).unwrap_or("Saved."));
                        fill(p);
                    }
                    // The parser's message, as the host gave it.
                    Err(e) => widgets::set_text(&error, &e.message),
                },
            );
        })
    };
    {
        let (put, view) = (put.clone(), view.clone());
        save.connect_clicked(move |_| {
            let b = view.buffer();
            put(Some(b.text(&b.start_iter(), &b.end_iter(), false).to_string()));
        });
    }
    {
        let (ctx2, put) = (ctx.clone(), put.clone());
        off.connect_clicked(move |_| {
            let put = put.clone();
            widgets::confirm(
                &ctx2.window,
                "Turn the policy off?",
                "Its file is kept. Everyone a link or pairing lets in may then watch and control.",
                "Turn off",
                None,
                move || put(None),
            );
        });
    }
    {
        let load = load.clone();
        reload.connect_clicked(move |_| load());
    }
    {
        let ctx2 = ctx.clone();
        let loaded_once = std::cell::Cell::new(false);
        ctx.listen(move |c| {
            let first_look = c == Change::Page && ctx2.page() == "policy" && !loaded_once.get();
            if ctx2.connected() && (c == Change::Link || first_look) {
                load();
                loaded_once.set(true);
            }
        });
    }
    pp
}

// Live sessions.

pub fn sessions_page(ctx: &Rc<Ctx>) -> adw::PreferencesPage {
    let pp = adw::PreferencesPage::new();
    let g = widgets::group(
        "Live sessions",
        "Each viewer with the policy's view of them: the rule that let them in, their groups, what they may do.",
    );
    let refresh = widgets::icon_button("view-refresh-symbolic", "Refresh the sessions");
    g.set_header_suffix(Some(&refresh));
    pp.add(&g);
    let error = widgets::error_label();
    let eg = adw::PreferencesGroup::new();
    eg.add(&error);
    pp.add(&eg);
    let rows = Rc::new(Rows::default());
    let load: Rc<dyn Fn()> = {
        let (ctx, g, rows, error) = (ctx.clone(), g.clone(), rows.clone(), error.clone());
        Rc::new(move || {
            let (g, rows, error) = (g.clone(), rows.clone(), error.clone());
            ctx.call(
                |c| c.sessions(),
                move |ctx, r| {
                    rows.clear(&g);
                    match r {
                        Ok(ss) => {
                            widgets::set_text(&error, "");
                            if ss.is_empty() {
                                rows.add(&g, &widgets::row("No sessions", ""));
                            }
                            for s in ss {
                                let mut sub = viewers::describe(&s);
                                sub.push_str(&format!("\nrule: {}", s.rule.as_deref().unwrap_or("none")));
                                if !s.groups.is_empty() {
                                    sub.push_str(&format!(" · groups: {}", s.groups.join(", ")));
                                }
                                let yn = |b: Option<bool>| match b {
                                    Some(true) => "yes",
                                    Some(false) => "no",
                                    None => "–",
                                };
                                sub.push_str(&format!(
                                    " · clipboard {} · sound {} · recorded {}",
                                    yn(s.clipboard),
                                    yn(s.audio),
                                    yn(s.recorded)
                                ));
                                if let Some(e) = &s.ends_at {
                                    sub.push_str(&format!(" · ends {}", time::short(e)));
                                }
                                let r = widgets::row(&viewers::title(&s), &sub);
                                r.set_subtitle_lines(4);
                                let b = widgets::button("Kick");
                                b.add_css_class("destructive-action");
                                widgets::name(&b, &format!("Kick session {}", viewers::title(&s)));
                                let ctx2 = ctx.clone();
                                b.connect_clicked(move |_| viewers::kick(&ctx2, &s, |_| {}));
                                r.add_suffix(&b);
                                rows.add(&g, &r);
                            }
                        }
                        Err(e) => widgets::set_text(&error, &e.message),
                    }
                },
            );
        })
    };
    {
        let load = load.clone();
        refresh.connect_clicked(move |_| load());
    }
    {
        let ctx2 = ctx.clone();
        // The viewers event comes every second while any are connected: their numbers, not just who.
        let last: RefCell<Vec<String>> = RefCell::default();
        ctx.listen(move |c| {
            if ctx2.page() != "sessions" || !ctx2.connected() {
                return;
            }
            let ids: Vec<String> = ctx2.viewers.borrow().iter().map(|v| v.id.clone()).collect();
            if c == Change::Page || c == Change::Link || (c == Change::Viewers && *last.borrow() != ids) {
                *last.borrow_mut() = ids;
                load();
            }
        });
    }
    pp
}

// The audit log.

pub fn audit_page(ctx: &Rc<Ctx>) -> adw::PreferencesPage {
    let pp = adw::PreferencesPage::new();
    let fg = widgets::group("Audit log", "Newest first. Choose events to see only those (none chosen: all).");
    pp.add(&fg);
    let flow = gtk::FlowBox::new();
    flow.set_selection_mode(gtk::SelectionMode::None);
    flow.set_max_children_per_line(4);
    flow.set_column_spacing(6);
    flow.set_row_spacing(2);
    let mut checks = Vec::new();
    for ev in AUDIT_EVENTS {
        let c = gtk::ToggleButton::with_label(ev);
        widgets::name(&c, &format!("Event {ev}"));
        flow.insert(&c, -1);
        checks.push((*ev, c));
    }
    let eg = widgets::group("Events", "");
    eg.add(&flow);
    let (ur, user) = widgets::entry_row("User", "Entries whose user contains this", "alice");
    let (sr, since) = widgets::entry_row("Since", "A date or a time (RFC 3339)", "2026-10-12");
    fg.add(&ur);
    fg.add(&sr);
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    bar.set_halign(gtk::Align::End);
    bar.set_margin_top(12);
    let search = widgets::button("Search");
    search.add_css_class("suggested-action");
    widgets::name(&search, "Search the audit log");
    bar.append(&search);
    let sg = adw::PreferencesGroup::new();
    sg.add(&bar);
    pp.add(&eg);
    pp.add(&sg);

    let rg = widgets::group("Entries", "");
    pp.add(&rg);
    let nav = adw::PreferencesGroup::new();
    let nbar = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    nbar.set_halign(gtk::Align::Center);
    let newest = widgets::button("Newest");
    widgets::name(&newest, "Newest entries");
    let older = widgets::button("Older");
    widgets::name(&older, "Older entries");
    nbar.append(&newest);
    nbar.append(&older);
    nav.add(&nbar);
    let error = widgets::error_label();
    nav.add(&error);
    pp.add(&nav);

    let checks = Rc::new(checks);
    let rows = Rc::new(Rows::default());
    let next: Rc<RefCell<Option<String>>> = Rc::default();
    let run: Rc<dyn Fn(Option<String>)> = {
        let (ctx, checks, user, since, rg, rows, next, older, error) = (
            ctx.clone(),
            checks.clone(),
            user.clone(),
            since.clone(),
            rg.clone(),
            rows.clone(),
            next.clone(),
            older.clone(),
            error.clone(),
        );
        Rc::new(move |before: Option<String>| {
            let q = AuditQuery {
                events: checks.iter().filter(|(_, c)| c.is_active()).map(|(e, _)| e.to_string()).collect(),
                user: user.text().to_string(),
                since: since.text().to_string(),
                limit: 50,
                before: before.clone(),
            };
            let (rg, rows, next, older, error) = (rg.clone(), rows.clone(), next.clone(), older.clone(), error.clone());
            ctx.call(
                move |c| c.audit(&q),
                move |_, r| {
                    rows.clear(&rg);
                    match r {
                        Ok(page) => {
                            widgets::set_text(&error, "");
                            rg.set_description(Some(&match (&page.path, before.is_some()) {
                                (None, _) => "The audit log is off (Settings, Audit log).".to_owned(),
                                (Some(p), false) => format!("{p}, the newest {}", page.entries.len()),
                                (Some(p), true) => format!("{p}, {} older", page.entries.len()),
                            }));
                            if page.entries.is_empty() {
                                rows.add(&rg, &widgets::row("No entries", ""));
                            }
                            for e in &page.entries {
                                let ev = value_text(e.get("event"));
                                let ts = time::short(&value_text(e.get("ts")));
                                let rest: Vec<String> = e
                                    .iter()
                                    .filter(|(k, _)| !matches!(k.as_str(), "event" | "ts"))
                                    .map(|(k, v)| format!("{k}: {}", value_text(Some(v))))
                                    .collect();
                                let r = widgets::row(&format!("{ev}  {ts}"), &rest.join(" · "));
                                r.set_subtitle_lines(3);
                                r.set_subtitle_selectable(true);
                                rows.add(&rg, &r);
                            }
                            older.set_sensitive(page.next.is_some());
                            *next.borrow_mut() = page.next;
                        }
                        Err(e) => widgets::set_text(&error, &e.message),
                    }
                },
            );
        })
    };
    {
        let run = run.clone();
        search.connect_clicked(move |_| run(None));
    }
    {
        let run = run.clone();
        newest.connect_clicked(move |_| run(None));
    }
    {
        let (run, next) = (run.clone(), next.clone());
        older.connect_clicked(move |_| {
            let n = next.borrow().clone();
            if n.is_some() {
                run(n);
            }
        });
    }
    {
        let ctx2 = ctx.clone();
        ctx.listen(move |c| {
            if c == Change::Page && ctx2.page() == "audit" && ctx2.connected() {
                run(None);
            }
        });
    }
    pp
}

// Metrics.

pub fn metrics_page(ctx: &Rc<Ctx>) -> adw::PreferencesPage {
    let pp = adw::PreferencesPage::new();
    let g = widgets::group(
        "Metrics",
        "Counted since the host started (across stops and starts of sharing); refreshed every 2 seconds.",
    );
    pp.add(&g);
    let error = widgets::error_label();
    let eg = adw::PreferencesGroup::new();
    eg.add(&error);
    pp.add(&eg);
    let rows = Rc::new(Rows::default());
    let busy = Rc::new(std::cell::Cell::new(false));
    let load = {
        let (ctx, g, rows, error, busy) = (ctx.clone(), g.clone(), rows.clone(), error.clone(), busy.clone());
        move || {
            if busy.get() || ctx.page() != "metrics" || !ctx.connected() {
                return;
            }
            busy.set(true);
            let (g, rows, error, busy) = (g.clone(), rows.clone(), error.clone(), busy.clone());
            ctx.call(
                |c| c.metrics(),
                move |_, r| {
                    busy.set(false);
                    match r {
                        Ok(m) => {
                            widgets::set_text(&error, "");
                            rows.clear(&g);
                            for (k, v) in m.rows() {
                                let r = widgets::row(&k.replace('_', " "), "");
                                let l = gtk::Label::new(Some(&v));
                                l.add_css_class("numeric");
                                l.set_selectable(true);
                                l.set_wrap(true);
                                r.add_suffix(&l);
                                rows.add(&g, &r);
                            }
                        }
                        Err(e) => widgets::set_text(&error, &e.message),
                    }
                },
            );
        }
    };
    let load = Rc::new(load);
    {
        let load = load.clone();
        ctx.listen(move |c| {
            if c == Change::Page || c == Change::Link {
                load();
            }
        });
    }
    glib::timeout_add_seconds_local(2, move || {
        load();
        glib::ControlFlow::Continue
    });
    pp
}
