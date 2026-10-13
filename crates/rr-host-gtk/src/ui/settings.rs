//! Settings: frame rate or quality first, bitrate, codec, frame rate, sound, the network (listen,
//! mDNS, pairing, relays), recording and the audit log, start at login. Saved together (only what
//! changed is sent), into `host.toml`.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use rr_host_gtk::model::{applied_text, value_text, Settings};
use serde_json::{Map, Value};

use super::widgets::{self, Rows};
use super::{Change, Ctx};

struct Form {
    framerate: gtk::CheckButton,
    quality: gtk::CheckButton,
    fps: gtk::Entry,
    bitrate: gtk::Entry,
    codecs: Vec<(&'static str, gtk::CheckButton)>,
    audio: gtk::Switch,
    listen: gtk::Entry,
    mdns: gtk::Switch,
    pair: gtk::Switch,
    webtransport: gtk::Switch,
    tailscale: gtk::Switch,
    relay_group: adw::PreferencesGroup,
    relay_rows: Rows,
    relays: RefCell<Vec<String>>,
    secret_row: adw::ActionRow,
    secret: gtk::PasswordEntry,
    secret_clear: RefCell<bool>,
    record: gtk::Entry,
    audit: gtk::Entry,
    overridden: adw::PreferencesGroup,
    error: gtk::Label,
    note: gtk::Label,
    /// The settings the form was filled from.
    loaded: RefCell<Map<String, Value>>,
}

/// A number when it is one, else the text (the host says what it accepts).
fn num_or_text(s: &str) -> Value {
    let s = s.trim();
    match s.parse::<u64>() {
        Ok(n) => Value::from(n),
        Err(_) => Value::from(s),
    }
}

impl Form {
    fn fill(self: &Rc<Self>, s: &Settings) {
        *self.loaded.borrow_mut() = s.settings.clone();
        if s.str("prefer") == "quality" {
            self.quality.set_active(true);
        } else {
            self.framerate.set_active(true);
        }
        self.fps.set_text(&s.str("fps"));
        self.bitrate.set_text(&s.str("bitrate"));
        let codec = s.str("codec");
        for (c, b) in &self.codecs {
            if *c == codec {
                b.set_active(true);
            }
        }
        self.audio.set_active(s.bool("audio"));
        self.listen.set_text(&s.str("listen"));
        self.mdns.set_active(s.bool("mdns"));
        self.pair.set_active(s.bool("pair"));
        self.webtransport.set_active(s.bool("webtransport"));
        self.tailscale.set_active(s.bool("tailscale-cert"));
        *self.relays.borrow_mut() = s.list("relay");
        self.draw_relays();
        self.secret.set_text("");
        *self.secret_clear.borrow_mut() = false;
        self.secret_row.set_subtitle(if s.bool("relay-secret-set") {
            "A secret is set (it's never shown). Type a new one to replace it."
        } else {
            "None set."
        });
        self.record.set_text(&s.str("record"));
        self.audit.set_text(&s.str("audit"));
        let o = s.overridden.join(", ");
        self.overridden.set_visible(!o.is_empty());
        self.overridden.set_description(Some(&format!(
            "Set on the host's command line for this run, and winning over host.toml until changed here: {o}"
        )));
    }

    /// The form's values, keyed as `host.toml` (secrets only when typed).
    fn values(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("prefer".into(), Value::from(if self.quality.is_active() { "quality" } else { "framerate" }));
        m.insert("fps".into(), num_or_text(&self.fps.text()));
        m.insert("bitrate".into(), num_or_text(&self.bitrate.text()));
        if let Some((c, _)) = self.codecs.iter().find(|(_, b)| b.is_active()) {
            m.insert("codec".into(), Value::from(*c));
        }
        m.insert("audio".into(), Value::from(self.audio.is_active()));
        m.insert("listen".into(), Value::from(self.listen.text().trim()));
        m.insert("mdns".into(), Value::from(self.mdns.is_active()));
        m.insert("pair".into(), Value::from(self.pair.is_active()));
        m.insert("webtransport".into(), Value::from(self.webtransport.is_active()));
        m.insert("tailscale-cert".into(), Value::from(self.tailscale.is_active()));
        m.insert("relay".into(), Value::from(self.relays.borrow().clone()));
        let rec = self.record.text().trim().to_owned();
        m.insert("record".into(), if rec.is_empty() { Value::Null } else { Value::from(rec) });
        let audit = self.audit.text().trim().to_owned();
        m.insert("audit".into(), if audit.is_empty() { Value::Null } else { Value::from(audit) });
        m
    }

    /// What changed since the form was filled: the PUT's body.
    fn changes(&self) -> Map<String, Value> {
        let loaded = self.loaded.borrow();
        let mut out = Map::new();
        for (k, v) in self.values() {
            let before = loaded.get(&k);
            let same = match (&v, before) {
                (Value::Array(a), Some(Value::Array(b))) => a == b,
                (Value::Array(a), None | Some(Value::Null)) => a.is_empty(),
                _ => value_text(Some(&v)) == value_text(before),
            };
            if !same {
                out.insert(k, v);
            }
        }
        let secret = self.secret.text().to_string();
        if !secret.is_empty() {
            out.insert("relay-secret".into(), Value::from(secret));
        } else if *self.secret_clear.borrow() {
            out.insert("relay-secret".into(), Value::Null);
        }
        out
    }

    fn draw_relays(self: &Rc<Self>) {
        self.relay_rows.clear(&self.relay_group);
        for (i, r) in self.relays.borrow().iter().enumerate() {
            let row = widgets::row(r, "");
            let rm = widgets::icon_button("list-remove-symbolic", &format!("Remove the relay {r}"));
            let f = self.clone();
            rm.connect_clicked(move |_| {
                f.relays.borrow_mut().remove(i);
                f.draw_relays();
            });
            row.add_suffix(&rm);
            self.relay_rows.add(&self.relay_group, &row);
        }
    }
}

pub fn page(ctx: &Rc<Ctx>) -> adw::PreferencesPage {
    let pp = adw::PreferencesPage::new();

    let overridden = widgets::group("From the command line", "");
    overridden.set_visible(false);
    pp.add(&overridden);

    let picture = widgets::group("Picture", "On a slow link something gives way: choose what.");
    let (r1, framerate) = widgets::choice_row(
        "Frame rate first",
        "The bitrate, then the picture's size give way before frames do (games, motion)",
        None,
    );
    let (r2, quality) = widgets::choice_row(
        "Quality first",
        "Frames give way first; the picture keeps its size (text, detail)",
        Some(&framerate),
    );
    picture.add(&r1);
    picture.add(&r2);
    let (fr, fps) = widgets::entry_row("Frame rate", "auto (the viewer's display rate) or 1 to 240", "auto");
    let (br, bitrate) = widgets::entry_row("Bitrate", "auto, or the most kbit/s to send", "auto");
    picture.add(&fr);
    picture.add(&br);
    pp.add(&picture);

    let codec_group = widgets::group("Codec", "");
    let mut codecs = Vec::new();
    let mut first: Option<gtk::CheckButton> = None;
    for (id, title, sub) in [
        ("h264", "H.264", "Every viewer decodes it"),
        ("h265", "H.265 (HEVC)", "Less bitrate for the same picture, where viewers have it"),
        ("auto", "Automatic", "HEVC where both sides have it in hardware"),
    ] {
        let (r, c) = widgets::choice_row(title, sub, first.as_ref());
        if first.is_none() {
            first = Some(c.clone());
        }
        codec_group.add(&r);
        codecs.push((id, c));
    }
    pp.add(&codec_group);

    let sound = widgets::group("Sound", "");
    let (ar, audio) = widgets::switch_row("Sound", "This computer's sound to viewers");
    sound.add(&ar);
    pp.add(&sound);

    let net = widgets::group("Network", "");
    let (lr, listen) = widgets::entry_row(
        "Listen",
        "auto (every interface, from port 7979) or ADDR:PORT (127.0.0.1:0: this computer only)",
        "auto",
    );
    let (mr, mdns) = widgets::switch_row("mDNS", "Found by name on the local network");
    let (pr, pair) = widgets::switch_row("Pairing", "Devices can pair with a code");
    let (wr, webtransport) = widgets::switch_row("WebTransport", "QUIC next to the WebSocket");
    let (tr, tailscale) = widgets::switch_row("Tailscale certificate", "A real certificate for the tailnet name");
    for r in [&lr, &mr, &pr, &wr, &tr] {
        net.add(r);
    }
    pp.add(&net);

    let relay_group = widgets::group("Relays", "For viewers on no shared network: the host waits on these (ws:// or wss://).");
    pp.add(&relay_group);
    let relay_add_group = adw::PreferencesGroup::new();
    let (rr, relay_entry) = widgets::entry_row("Add a relay", "", "wss://relay.example");
    let add = widgets::button("Add");
    widgets::name(&add, "Add the relay");
    rr.add_suffix(&add);
    relay_add_group.add(&rr);
    let secret_row = widgets::row("Relay secret", "");
    let secret = gtk::PasswordEntry::new();
    secret.set_show_peek_icon(true);
    secret.set_valign(gtk::Align::Center);
    secret.set_width_chars(22);
    widgets::name(&secret, "Relay secret");
    let clear = widgets::button("Clear");
    widgets::name(&clear, "Clear the relay secret");
    secret_row.add_suffix(&secret);
    secret_row.add_suffix(&clear);
    relay_add_group.add(&secret_row);
    pp.add(&relay_add_group);

    let rec = widgets::group("Recording and audit", "");
    let (recr, record) = widgets::entry_row("Record to", "A folder to record sessions to (MP4); empty: off", "");
    let (aur, audit) = widgets::entry_row("Audit log", "The audit log's file, or off; empty: the default", "");
    rec.add(&recr);
    rec.add(&aur);
    pp.add(&rec);

    let save_group = adw::PreferencesGroup::new();
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    bar.set_halign(gtk::Align::End);
    let revert = widgets::button("Revert");
    widgets::name(&revert, "Revert the settings");
    let save = widgets::button("Save");
    save.add_css_class("suggested-action");
    widgets::name(&save, "Save settings");
    bar.append(&revert);
    bar.append(&save);
    let error = widgets::error_label();
    let note = widgets::note_label();
    save_group.add(&bar);
    save_group.add(&error);
    save_group.add(&note);
    pp.add(&save_group);

    // Start at login: the host (its user service, through the API) and this app (XDG autostart).
    let login = widgets::group("Start at login", "");
    let (hr, host_login) = widgets::switch_row("Start the host at login", "");
    let (appr, app_login) = widgets::switch_row("Show Rein Remote in the tray at login", "This app, without its window");
    login.add(&hr);
    login.add(&appr);
    let login_error = widgets::error_label();
    login.add(&login_error);
    pp.add(&login);

    let f = Rc::new(Form {
        framerate,
        quality,
        fps,
        bitrate,
        codecs,
        audio,
        listen,
        mdns,
        pair,
        webtransport,
        tailscale,
        relay_group,
        relay_rows: Rows::default(),
        relays: RefCell::new(vec![]),
        secret_row,
        secret,
        secret_clear: RefCell::new(false),
        record,
        audit,
        overridden,
        error,
        note,
        loaded: RefCell::new(Map::new()),
    });

    {
        let (f2, e2) = (f.clone(), relay_entry.clone());
        let go = Rc::new(move || {
            let r = e2.text().trim().to_owned();
            if !r.is_empty() && !f2.relays.borrow().contains(&r) {
                f2.relays.borrow_mut().push(r);
                f2.draw_relays();
                e2.set_text("");
            }
        });
        let g2 = go.clone();
        add.connect_clicked(move |_| g2());
        relay_entry.connect_activate(move |_| go());
    }
    {
        let f2 = f.clone();
        clear.connect_clicked(move |_| {
            *f2.secret_clear.borrow_mut() = true;
            f2.secret.set_text("");
            f2.secret_row.set_subtitle("Cleared when you save.");
        });
    }
    {
        let (ctx2, f2) = (ctx.clone(), f.clone());
        revert.connect_clicked(move |_| {
            f2.fill(&ctx2.settings.borrow());
            widgets::set_text(&f2.error, "");
            widgets::set_text(&f2.note, "");
        });
    }
    {
        let (ctx2, f2) = (ctx.clone(), f.clone());
        save.connect_clicked(move |_| {
            let changes = f2.changes();
            widgets::set_text(&f2.error, "");
            if changes.is_empty() {
                widgets::set_text(&f2.note, "Nothing changed.");
                return;
            }
            let f3 = f2.clone();
            ctx2.call(
                move |c| c.put_settings(&changes),
                move |ctx, r| match r {
                    Ok(s) => {
                        widgets::set_text(&f3.note, applied_text(s.applied.as_deref()).unwrap_or("Saved."));
                        f3.fill(&s);
                        ctx.set_settings(s);
                    }
                    Err(e) => {
                        widgets::set_text(&f3.error, &e.message);
                        widgets::set_text(&f3.note, "");
                    }
                },
            );
        });
    }

    // Start at login.
    let host_login_quiet = Rc::new(std::cell::Cell::new(false));
    {
        let (ctx2, q, le) = (ctx.clone(), host_login_quiet.clone(), login_error.clone());
        let hr2 = hr.clone();
        host_login.connect_active_notify(move |s| {
            if q.get() {
                return;
            }
            let (on, s2, q2, le2, hr3) = (s.is_active(), s.clone(), q.clone(), le.clone(), hr2.clone());
            ctx2.call(
                move |c| c.set_start_at_login(on),
                move |_, r| {
                    q2.set(true);
                    match r {
                        Ok(sal) => {
                            s2.set_active(sal.enabled);
                            hr3.set_subtitle(&login_text(&sal));
                            widgets::set_text(&le2, "");
                        }
                        Err(e) => {
                            s2.set_active(!on);
                            widgets::set_text(&le2, &e.message);
                        }
                    }
                    q2.set(false);
                },
            );
        });
    }
    app_login.set_active(autostart_path().is_some_and(|p| p.exists()));
    {
        let le = login_error.clone();
        app_login.connect_active_notify(move |s| {
            if let Err(e) = set_autostart(s.is_active()) {
                widgets::set_text(&le, &e);
            }
        });
    }

    {
        let (ctx2, f2, q, hl) = (ctx.clone(), f.clone(), host_login_quiet, host_login.clone());
        ctx.listen(move |c| match c {
            Change::Settings => {
                // Refilled unless there are edits not saved yet.
                if f2.changes().is_empty() || f2.loaded.borrow().is_empty() {
                    f2.fill(&ctx2.settings.borrow());
                }
            }
            Change::Link if ctx2.connected() => {
                let (q, hl, hr) = (q.clone(), hl.clone(), hr.clone());
                ctx2.call(
                    |c| c.start_at_login(),
                    move |_, r| {
                        if let Ok(sal) = r {
                            q.set(true);
                            hl.set_active(sal.enabled);
                            hr.set_subtitle(&login_text(&sal));
                            q.set(false);
                        }
                    },
                );
            }
            _ => {}
        });
    }
    pp
}

fn login_text(s: &rr_host_gtk::model::StartAtLogin) -> String {
    match (&s.kind, &s.path) {
        (Some(k), Some(p)) => format!("{k}: {p}"),
        (Some(k), None) => k.clone(),
        _ => "rein-remote host as your user's service".into(),
    }
}

/// `~/.config/autostart/dev.reinharness.RemoteHost.desktop`.
fn autostart_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("autostart").join(format!("{}.desktop", super::APP_ID)))
}

fn set_autostart(on: bool) -> Result<(), String> {
    let path = autostart_path().ok_or("no home folder")?;
    if !on {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("{}: {e}", path.display())),
            _ => Ok(()),
        };
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let text = format!(
        "[Desktop Entry]\nType=Application\nName=Rein Remote\nComment=Share this screen: the tray icon\nExec=\"{}\" --hidden\nIcon={}\nX-GNOME-Autostart-enabled=true\nNoDisplay=true\n",
        exe.display(),
        super::APP_ID
    );
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
}
