//! Viewers: who's watching now, each with Kick (asked first).

use std::rc::Rc;

use adw::prelude::*;
use rr_host_gtk::model::Viewer;
use rr_host_gtk::time;

use super::widgets::{self, Rows};
use super::{Change, Ctx};

/// A viewer's line: who, how, and how it's going.
pub fn describe(v: &Viewer) -> String {
    let mut parts = vec![];
    if !v.user.is_empty() && v.user != v.device {
        parts.push(v.user.clone());
    }
    parts.push(format!("via {}", v.via));
    parts.push(v.transport.clone());
    if !v.addr.is_empty() {
        parts.push(v.addr.clone());
    }
    parts.push(format!("{:.0} fps", v.fps));
    if let Some(r) = v.rtt_ms {
        parts.push(format!("{r:.0} ms"));
    }
    if v.width > 0 {
        parts.push(format!("{}×{}", v.width, v.height));
    }
    if let Some(k) = v.kbps {
        parts.push(format!("{:.1} Mbit/s", k / 1000.0));
    }
    if v.view_only {
        parts.push("view only".into());
    }
    if !v.since.is_empty() {
        parts.push(format!("since {}", time::ago(&v.since, time::now())));
    }
    parts.join(" · ")
}

pub fn title(v: &Viewer) -> String {
    if !v.device.is_empty() {
        v.device.clone()
    } else if !v.user.is_empty() {
        v.user.clone()
    } else {
        format!("Viewer {}", v.id)
    }
}

/// Kick, asked first; `after` with the viewers left.
pub fn kick(ctx: &Rc<Ctx>, v: &Viewer, after: impl Fn(&Rc<Ctx>) + 'static) {
    let reason = gtk::Entry::new();
    reason.set_placeholder_text(Some("Reason (optional, shown to them)"));
    widgets::name(&reason, "Reason");
    let (ctx2, id, who, r2) = (ctx.clone(), v.id.clone(), title(v), reason.clone());
    let after = Rc::new(after);
    widgets::confirm(
        &ctx.window,
        &format!("Kick {who}?"),
        "Their session ends now. They can connect again with what they have (their link, their pairing): to keep a device out, unpair it.",
        "Kick viewer",
        Some(reason.upcast_ref()),
        move || {
            let id = id.clone();
            let reason = r2.text().to_string();
            let who = who.clone();
            let after = after.clone();
            ctx2.call(
                move |c| c.kick(&id, Some(&reason)),
                move |ctx, r| match r {
                    Ok(v) => {
                        ctx.set_viewers(v);
                        ctx.toast(&format!("{who} was kicked."));
                        after(ctx);
                    }
                    Err(e) => ctx.toast(&e.message),
                },
            );
        },
    );
}

pub fn page(ctx: &Rc<Ctx>) -> adw::PreferencesPage {
    let pp = adw::PreferencesPage::new();
    let g = widgets::group(
        "Watching now",
        "One row per session. Kicking ends it; they may connect again with their link or pairing.",
    );
    pp.add(&g);
    let rows = Rc::new(Rows::default());
    let draw = {
        let (ctx, g, rows) = (ctx.clone(), g.clone(), rows.clone());
        move || {
            rows.clear(&g);
            let vs = ctx.viewers.borrow().clone();
            if vs.is_empty() {
                let r = widgets::row(
                    if !ctx.connected() {
                        "Not connected to the host"
                    } else if ctx.state.borrow().sharing {
                        "No one is watching"
                    } else {
                        "Not sharing"
                    },
                    "",
                );
                rows.add(&g, &r);
                return;
            }
            for v in vs {
                let r = widgets::row(&title(&v), &describe(&v));
                r.set_subtitle_lines(3);
                let b = widgets::button("Kick");
                b.add_css_class("destructive-action");
                widgets::name(&b, &format!("Kick {}", title(&v)));
                let ctx2 = ctx.clone();
                b.connect_clicked(move |_| kick(&ctx2, &v, |_| {}));
                r.add_suffix(&b);
                rows.add(&g, &r);
            }
        }
    };
    draw();
    ctx.listen(move |c| {
        if matches!(c, Change::Viewers | Change::Link | Change::State) {
            draw()
        }
    });
    pp
}
