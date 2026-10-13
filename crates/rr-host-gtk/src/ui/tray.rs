//! The tray icon: a StatusNotifierItem (KDE, and GNOME with the AppIndicator extension) through
//! ksni, with a menu: the status, start or stop, the pairing code, the viewers, Open, Quit.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use gtk::glib;
use ksni::blocking::TrayMethods;
use ksni::menu::{MenuItem, StandardItem};

use super::TrayCmd;

/// What the tray shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrayState {
    pub status: String,
    pub connected: bool,
    pub sharing: bool,
    pub code: Option<String>,
    pub viewers: u32,
}

pub struct ReinTray {
    state: TrayState,
    tx: async_channel::Sender<TrayCmd>,
    /// Whether a StatusNotifierWatcher (a panel that shows tray icons) is there.
    online: Arc<AtomicBool>,
}

/// The running tray, and a thread that gives it what to show (ksni's update waits on its own
/// thread's executor, so not on the main thread).
pub struct TrayHandle {
    handle: ksni::blocking::Handle<ReinTray>,
    tx: std::sync::mpsc::Sender<TrayState>,
}

impl ReinTray {
    fn send(&self, c: TrayCmd) {
        let _ = self.tx.send_blocking(c);
    }
}

impl ksni::Tray for ReinTray {
    fn id(&self) -> String {
        "rein-remote-host".into()
    }

    fn title(&self) -> String {
        "Rein Remote".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::ApplicationStatus
    }

    fn icon_name(&self) -> String {
        String::new()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        [22, 32, 48].iter().map(|&s| icon(s, self.state.sharing, self.state.connected)).collect()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "Rein Remote".into(),
            description: self.state.status.clone(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(TrayCmd::Open);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let s = &self.state;
        let mut items: Vec<MenuItem<Self>> = vec![StandardItem {
            label: s.status.replace('_', "__"),
            enabled: false,
            ..Default::default()
        }
        .into()];
        if s.connected {
            items.push(
                StandardItem {
                    label: if s.sharing { "Stop sharing".into() } else { "Start sharing".into() },
                    activate: Box::new(|t: &mut Self| {
                        let cmd = if t.state.sharing { TrayCmd::Stop } else { TrayCmd::Start };
                        t.send(cmd)
                    }),
                    ..Default::default()
                }
                .into(),
            );
            if s.sharing {
                items.push(
                    StandardItem {
                        label: match &s.code {
                            Some(c) => format!("Pairing code: {c}"),
                            None => "Pairing code: none".into(),
                        },
                        enabled: false,
                        ..Default::default()
                    }
                    .into(),
                );
                items.push(
                    StandardItem {
                        label: "New pairing code".into(),
                        activate: Box::new(|t: &mut Self| t.send(TrayCmd::NewCode)),
                        ..Default::default()
                    }
                    .into(),
                );
            }
            items.push(
                StandardItem {
                    label: match s.viewers {
                        1 => "1 viewer".into(),
                        n => format!("{n} viewers"),
                    },
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }
        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Open Rein Remote".into(),
                activate: Box::new(|t: &mut Self| t.send(TrayCmd::Open)),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|t: &mut Self| t.send(TrayCmd::Quit)),
                ..Default::default()
            }
            .into(),
        );
        items
    }

    fn watcher_online(&self) {
        self.online.store(true, Ordering::Relaxed);
    }

    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        self.online.store(false, Ordering::Relaxed);
        // Keep the item on the bus: a panel that starts later picks it up.
        true
    }
}

/// The tray icon, on D-Bus whether or not a panel shows it now. `online` follows whether one does
/// (checked on the main thread every second).
pub fn spawn(tx: async_channel::Sender<TrayCmd>, online: Rc<Cell<bool>>) -> Result<TrayHandle, String> {
    let flag = Arc::new(AtomicBool::new(true));
    let tray = ReinTray {
        state: TrayState {
            status: "Connecting…".into(),
            ..Default::default()
        },
        tx,
        online: flag.clone(),
    };
    let handle = tray.assume_sni_available(true).spawn().map_err(|e| e.to_string())?;
    let (utx, urx) = std::sync::mpsc::channel::<TrayState>();
    let h = handle.clone();
    std::thread::spawn(move || {
        while let Ok(mut s) = urx.recv() {
            // Only the latest matters.
            while let Ok(newer) = urx.try_recv() {
                s = newer;
            }
            h.update(|t| {
                if t.state != s {
                    t.state = s;
                }
            });
        }
    });
    online.set(flag.load(Ordering::Relaxed));
    glib::timeout_add_seconds_local(1, move || {
        online.set(flag.load(Ordering::Relaxed));
        glib::ControlFlow::Continue
    });
    Ok(TrayHandle { handle, tx: utx })
}

pub fn update(h: &TrayHandle, s: TrayState) {
    let _ = h.tx.send(s);
}

pub fn shutdown(h: TrayHandle) {
    h.handle.shutdown().wait();
}

/// The icon, drawn: a screen (blue while sharing, grey when not, a red bar when the host is gone)
/// with a green dot while sharing. ARGB32, big-endian, as the spec asks.
fn icon(size: i32, sharing: bool, connected: bool) -> ksni::Icon {
    let s = size as f32;
    let mut data = vec![0u8; (size * size * 4) as usize];
    let frame: [u8; 3] = if !connected {
        [0x9a, 0x9a, 0x9a]
    } else if sharing {
        [0x35, 0x84, 0xe4]
    } else {
        [0x77, 0x76, 0x7b]
    };
    let mut put = |x: i32, y: i32, c: [u8; 3]| {
        if x >= 0 && y >= 0 && x < size && y < size {
            let p = ((y * size + x) * 4) as usize;
            data[p..p + 4].copy_from_slice(&[0xff, c[0], c[1], c[2]]);
        }
    };
    let (l, r, t, b) = ((s * 0.08) as i32, (s * 0.92) as i32, (s * 0.14) as i32, (s * 0.70) as i32);
    let w = (s / 11.0).max(2.0) as i32;
    for y in t..b {
        for x in l..r {
            let edge = x < l + w || x >= r - w || y < t + w || y >= b - w;
            let screen = if sharing { [0xd0, 0xe4, 0xfb] } else { [0xee, 0xee, 0xee] };
            put(x, y, if edge { frame } else { screen });
        }
    }
    // The stand.
    let cx = size / 2;
    for y in b..(s * 0.84) as i32 {
        for x in cx - w..cx + w {
            put(x, y, frame);
        }
    }
    for y in (s * 0.84) as i32..(s * 0.84) as i32 + w {
        for x in (s * 0.3) as i32..(s * 0.7) as i32 {
            put(x, y, frame);
        }
    }
    if !connected {
        for y in (s * 0.38) as i32..(s * 0.38) as i32 + w {
            for x in l + 2 * w..r - 2 * w {
                put(x, y, [0xe0, 0x1b, 0x24]);
            }
        }
    } else if sharing {
        let (dx, dy, rad) = (s * 0.78, s * 0.24, s * 0.2);
        for y in 0..size {
            for x in 0..size {
                let (fx, fy) = (x as f32 + 0.5 - dx, y as f32 + 0.5 - dy);
                if fx * fx + fy * fy <= rad * rad {
                    put(x, y, [0x2e, 0xc2, 0x7e]);
                }
            }
        }
    }
    ksni::Icon {
        width: size,
        height: size,
        data,
    }
}
