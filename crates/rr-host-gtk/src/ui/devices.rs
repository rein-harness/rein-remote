//! Paired devices: each with Unpair (asked first).

use std::rc::Rc;

use adw::prelude::*;
use rr_host_gtk::time;

use super::widgets::{self, Rows};
use super::{Change, Ctx};

pub fn page(ctx: &Rc<Ctx>) -> adw::PreferencesPage {
    let pp = adw::PreferencesPage::new();
    let g = widgets::group(
        "Paired devices",
        "Devices that paired with a code connect with their own key. Unpairing ends their sessions; they must pair again with a new code.",
    );
    pp.add(&g);
    let rows = Rc::new(Rows::default());
    let draw = {
        let (ctx, g, rows) = (ctx.clone(), g.clone(), rows.clone());
        move || {
            rows.clear(&g);
            let ds = ctx.devices.borrow().clone();
            if ds.is_empty() {
                let r = widgets::row(if ctx.connected() { "No paired devices" } else { "Not connected to the host" }, "");
                rows.add(&g, &r);
                return;
            }
            for d in ds {
                let since = if d.since.is_empty() {
                    String::new()
                } else {
                    format!(" · paired {}", time::short(&d.since))
                };
                let r = widgets::row(&d.name, &format!("{}{since}", d.id));
                r.set_subtitle_selectable(true);
                let b = widgets::button("Unpair");
                b.add_css_class("destructive-action");
                widgets::name(&b, &format!("Unpair {}", d.name));
                let ctx2 = ctx.clone();
                b.connect_clicked(move |_| {
                    let (ctx3, id, name) = (ctx2.clone(), d.id.clone(), d.name.clone());
                    widgets::confirm(
                        &ctx2.window,
                        &format!("Unpair {}?", d.name),
                        "Its open sessions end, and it must pair again with a new code to connect.",
                        "Unpair",
                        None,
                        move || {
                            let (id, name) = (id.clone(), name.clone());
                            ctx3.call(
                                move |c| c.unpair(&id),
                                move |ctx, r| match r {
                                    Ok(d) => {
                                        ctx.set_devices(d);
                                        ctx.toast(&format!("{name} was unpaired."));
                                    }
                                    Err(e) => ctx.toast(&e.message),
                                },
                            );
                        },
                    );
                });
                r.add_suffix(&b);
                rows.add(&g, &r);
            }
        }
    };
    draw();
    ctx.listen(move |c| {
        if matches!(c, Change::Devices | Change::Link) {
            draw()
        }
    });
    pp
}
