//! The app: one window (a sidebar of pages), a tray icon, and a thread that follows the host.

mod admin;
mod devices;
mod settings;
mod sharing;
mod tray;
mod viewers;
mod widgets;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gio, glib};
use rr_host_gtk::client::{ApiError, ApiResult, Client};
use rr_host_gtk::launch::{self, Link, Msg, Options, Started};
use rr_host_gtk::model::*;

pub const APP_ID: &str = "dev.reinharness.RemoteHost";

/// What changed, for the pages to redraw what they show of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Link,
    State,
    Viewers,
    Devices,
    Settings,
    /// The page shown changed (pages that poll start or stop).
    Page,
}

type Listener = Rc<dyn Fn(Change)>;
type ActionFn = Box<dyn Fn(&Rc<Ctx>, Option<&glib::Variant>)>;

/// What the tray's menu asks for (from the tray's thread).
#[derive(Clone, Copy, Debug)]
pub enum TrayCmd {
    Open,
    Start,
    Stop,
    NewCode,
    Quit,
}

/// The app's state, shared by its pages (on the main thread).
pub struct Ctx {
    pub app: adw::Application,
    pub window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    banner: adw::Banner,
    stack: gtk::Stack,
    sidebar: gtk::ListBox,
    split: adw::NavigationSplitView,
    content_page: adw::NavigationPage,
    pub client: RefCell<Option<Client>>,
    pub link: RefCell<Link>,
    pub state: RefCell<State>,
    pub viewers: RefCell<Vec<Viewer>>,
    pub devices: RefCell<Vec<Device>>,
    pub settings: RefCell<Settings>,
    listeners: RefCell<Vec<Listener>>,
    pages: RefCell<Vec<(&'static str, &'static str)>>,
    tray: RefCell<Option<tray::TrayHandle>>,
    pub tray_online: Rc<Cell<bool>>,
    started: Started,
    stop: Arc<AtomicBool>,
}

impl Ctx {
    /// Calls the host off the main thread; `done` gets the answer on it. Errors are shown as a
    /// toast too, unless `done` says it showed them (returns true).
    pub fn call<T: Send + 'static>(
        self: &Rc<Self>,
        f: impl FnOnce(&Client) -> ApiResult<T> + Send + 'static,
        done: impl FnOnce(&Rc<Ctx>, ApiResult<T>) + 'static,
    ) {
        let ctx = self.clone();
        let Some(client) = self.client.borrow().clone() else {
            done(
                &ctx,
                Err(ApiError {
                    status: 0,
                    message: "Not connected to rein-remote host.".into(),
                }),
            );
            return;
        };
        glib::spawn_future_local(async move {
            let r = gio::spawn_blocking(move || f(&client)).await.unwrap_or_else(|_| {
                Err(ApiError {
                    status: 0,
                    message: "the call to the host failed".into(),
                })
            });
            done(&ctx, r);
        });
    }

    pub fn toast(&self, text: &str) {
        let t = adw::Toast::new(&glib::markup_escape_text(text));
        t.set_timeout(4);
        self.toasts.add_toast(t);
    }

    pub fn listen(&self, f: impl Fn(Change) + 'static) {
        self.listeners.borrow_mut().push(Rc::new(f));
    }

    fn changed(&self, c: Change) {
        let ls: Vec<_> = self.listeners.borrow().clone();
        for l in ls {
            l(c);
        }
        if matches!(c, Change::State | Change::Link | Change::Viewers) {
            self.update_tray();
            self.update_title();
        }
    }

    pub fn connected(&self) -> bool {
        matches!(*self.link.borrow(), Link::Connected { .. })
    }

    pub fn page(&self) -> String {
        self.stack.visible_child_name().map(|s| s.to_string()).unwrap_or_default()
    }

    pub fn show_page(&self, id: &str) {
        let pages = self.pages.borrow();
        if let Some(i) = pages.iter().position(|(p, _)| *p == id) {
            self.stack.set_visible_child_name(id);
            self.content_page.set_title(pages[i].1);
            // The "Admin" heading is a row of its own, after the fourth page.
            let row = if i >= 4 { i + 1 } else { i };
            if let Some(r) = self.sidebar.row_at_index(row as i32) {
                self.sidebar.select_row(Some(&r));
            }
            self.split.set_show_content(true);
        }
    }

    pub fn present(&self) {
        self.window.set_visible(true);
        self.window.present();
    }

    /// The state as the host just answered it (after a start, a stop, a source switch).
    pub fn set_state(&self, s: State) {
        *self.state.borrow_mut() = s;
        self.changed(Change::State);
    }

    pub fn set_viewers(&self, v: Vec<Viewer>) {
        *self.viewers.borrow_mut() = v;
        self.changed(Change::Viewers);
    }

    pub fn set_devices(&self, d: Vec<Device>) {
        *self.devices.borrow_mut() = d;
        self.changed(Change::Devices);
    }

    pub fn set_settings(&self, s: Settings) {
        *self.settings.borrow_mut() = s;
        self.changed(Change::Settings);
    }

    pub fn status_text(&self) -> String {
        match &*self.link.borrow() {
            Link::Connected { .. } => {
                let s = self.state.borrow();
                match (s.sharing, s.viewers) {
                    (false, _) => "Not sharing".into(),
                    (true, 0) => "Sharing, no one watching".into(),
                    (true, 1) => "Sharing, 1 watching".into(),
                    (true, n) => format!("Sharing, {n} watching"),
                }
            }
            Link::Connecting => "Connecting to rein-remote host…".into(),
            Link::Starting(_) => "Starting rein-remote host…".into(),
            Link::Gone(_) => "rein-remote host isn't running".into(),
        }
    }

    fn update_title(&self) {
        self.window.set_title(Some(&format!("Rein Remote: {}", self.status_text())));
    }

    fn update_tray(&self) {
        if let Some(t) = &*self.tray.borrow() {
            let s = self.state.borrow();
            tray::update(
                t,
                tray::TrayState {
                    status: self.status_text(),
                    connected: self.connected(),
                    sharing: s.sharing,
                    code: s.pairing.code.clone(),
                    viewers: s.viewers,
                },
            );
        }
    }

    /// Start or stop sharing (the Sharing page's button, the tray, the app's actions).
    pub fn set_sharing(self: &Rc<Self>, on: bool) {
        self.call(
            move |c| if on { c.start() } else { c.stop(None) },
            |ctx, r| match r {
                Ok(s) => ctx.set_state(s),
                Err(e) => ctx.toast(&e.message),
            },
        );
    }

    pub fn new_code(self: &Rc<Self>) {
        self.call(
            |c| c.new_code(),
            |ctx, r| match r {
                Ok(p) => {
                    let mut s = ctx.state.borrow().clone();
                    s.pairing = p;
                    ctx.set_state(s);
                }
                Err(e) => ctx.toast(&e.message),
            },
        );
    }

    fn quit(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.tray.borrow_mut().take() {
            tray::shutdown(t);
        }
        // The host this app started goes with it; one started otherwise (at login, by hand) stays.
        self.started.stop();
        self.app.quit();
    }

    fn on_msg(self: &Rc<Self>, m: Msg) {
        match m {
            Msg::Link(l) => {
                let banner = match &l {
                    Link::Connected { .. } => None,
                    Link::Connecting => Some("Connecting to rein-remote host…".to_owned()),
                    Link::Starting(s) => Some(s.clone()),
                    Link::Gone(why) => Some(why.clone()),
                };
                if let Link::Connected { client, state, .. } = &l {
                    *self.client.borrow_mut() = Some(client.clone());
                    *self.state.borrow_mut() = (**state).clone();
                } else {
                    // Nothing the host said before holds now (its links died with it).
                    *self.client.borrow_mut() = None;
                    *self.state.borrow_mut() = State::default();
                    self.viewers.borrow_mut().clear();
                }
                *self.link.borrow_mut() = l;
                match banner {
                    Some(b) => {
                        self.banner.set_title(&glib::markup_escape_text(&b));
                        self.banner.set_revealed(true);
                    }
                    None => {
                        self.banner.set_revealed(false);
                        // (A hidden banner is still in the accessibility tree: no stale text there.)
                        self.banner.set_title("");
                    }
                }
                self.changed(Change::Link);
                self.changed(Change::State);
            }
            Msg::Event(Event::State(s)) => self.set_state(s),
            Msg::Event(Event::Viewers(v)) => self.set_viewers(v),
            Msg::Event(Event::Devices(d)) => self.set_devices(d),
            Msg::Event(Event::Settings(s)) => self.set_settings(s),
            Msg::Event(Event::Other(_)) => {}
        }
    }
}

/// The app's own options (GTK's are not used).
struct Args {
    hidden: bool,
    start_host: bool,
    host_args: Vec<String>,
    page: Option<String>,
    binary: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        hidden: false,
        start_host: true,
        host_args: vec![],
        page: None,
        binary: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--hidden" => a.hidden = true,
            "--no-start" => a.start_host = false,
            "--host-arg" => a.host_args.push(it.next().ok_or("--host-arg needs a value")?),
            "--page" => a.page = Some(it.next().ok_or("--page needs a value")?),
            "--rein-remote" => a.binary = Some(it.next().ok_or("--rein-remote needs a path")?.into()),
            "--version" => {
                println!("rein-remote-host-gtk {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "-h" | "--help" => {
                println!(
                    "rein-remote-host-gtk: the UI for `rein-remote host`\n\n  --hidden            start in the tray, no window\n  --no-start          don't start `rein-remote host` when none runs (wait for one)\n  --host-arg ARG      an argument for `rein-remote host` when this app starts it (repeat)\n  --rein-remote PATH  the rein-remote binary (else next to this app, else on PATH)\n  --page NAME         open on a page: sharing, viewers, devices, settings, signin, policy, sessions, audit, metrics\n\nThe host's home folder: $REIN_REMOTE_HOME, else ~/.rein-remote."
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown option {other} (see --help)")),
        }
    }
    Ok(a)
}

pub fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("rein-remote-host-gtk: {e}");
            return ExitCode::from(2);
        }
    };
    glib::set_prgname(Some("rein-remote-host-gtk"));
    glib::set_application_name("Rein Remote");
    let app = adw::Application::builder().application_id(APP_ID).build();
    let args = Rc::new(RefCell::new(Some(args)));
    let ctx_cell: Rc<RefCell<Option<Rc<Ctx>>>> = Rc::new(RefCell::new(None));
    {
        let ctx_cell = ctx_cell.clone();
        app.connect_activate(move |app| {
            if let Some(ctx) = &*ctx_cell.borrow() {
                // A second start of the app: the first one's window comes forward.
                ctx.present();
                return;
            }
            let Some(args) = args.borrow_mut().take() else { return };
            let ctx = build(app, &args);
            *ctx_cell.borrow_mut() = Some(ctx);
        });
    }
    let code = app.run_with_args(&[std::env::args().next().unwrap_or_default()]);
    if let Some(ctx) = ctx_cell.borrow_mut().take() {
        ctx.stop.store(true, Ordering::Relaxed);
        ctx.started.stop();
    }
    ExitCode::from(code.get() as u8)
}

const PAGES: &[(&str, &str, &str)] = &[
    ("sharing", "Sharing", "video-display-symbolic"),
    ("viewers", "Viewers", "system-users-symbolic"),
    ("devices", "Paired devices", "phone-symbolic"),
    ("settings", "Settings", "emblem-system-symbolic"),
    ("signin", "Sign-in", "dialog-password-symbolic"),
    ("policy", "Policy", "document-edit-symbolic"),
    ("sessions", "Live sessions", "network-transmit-receive-symbolic"),
    ("audit", "Audit log", "view-list-symbolic"),
    ("metrics", "Metrics", "preferences-system-details-symbolic"),
];

fn build(app: &adw::Application, args: &Args) -> Rc<Ctx> {
    // The app's icon, from where it's installed (or next to the binary, or the source tree).
    if let Some(display) = gtk::gdk::Display::default() {
        let theme = gtk::IconTheme::for_display(&display);
        if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf())) {
            theme.add_search_path(dir.join("../share/icons"));
            theme.add_search_path(dir.join("icons"));
        }
        theme.add_search_path(concat!(env!("CARGO_MANIFEST_DIR"), "/data/icons"));
    }
    gtk::Window::set_default_icon_name(APP_ID);

    let toasts = adw::ToastOverlay::new();
    let banner = adw::Banner::new("Connecting to rein-remote host…");
    banner.set_revealed(true);
    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    let sidebar = gtk::ListBox::new();
    sidebar.add_css_class("navigation-sidebar");
    widgets::name(&sidebar, "Pages");

    let content = adw::ToolbarView::new();
    content.add_top_bar(&adw::HeaderBar::new());
    content.add_top_bar(&banner);
    content.set_content(Some(&stack));
    let content_page = adw::NavigationPage::new(&content, "Sharing");
    content_page.set_tag(Some("content"));

    let side = adw::ToolbarView::new();
    side.add_top_bar(&adw::HeaderBar::new());
    let side_scroll = gtk::ScrolledWindow::new();
    side_scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
    side_scroll.set_child(Some(&sidebar));
    side.set_content(Some(&side_scroll));
    let side_page = adw::NavigationPage::new(&side, "Rein Remote");

    let split = adw::NavigationSplitView::new();
    split.set_sidebar(Some(&side_page));
    split.set_content(Some(&content_page));
    split.set_min_sidebar_width(200.0);
    toasts.set_child(Some(&split));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Rein Remote")
        .default_width(980)
        .default_height(760)
        .width_request(360)
        .height_request(480)
        .content(&toasts)
        .build();
    let bp = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 640sp").unwrap());
    bp.add_setter(&split, "collapsed", Some(&true.to_value()));
    window.add_breakpoint(bp);

    let ctx = Rc::new(Ctx {
        app: app.clone(),
        window: window.clone(),
        toasts,
        banner,
        stack: stack.clone(),
        sidebar: sidebar.clone(),
        split,
        content_page,
        client: RefCell::new(None),
        link: RefCell::new(Link::Connecting),
        state: RefCell::new(State::default()),
        viewers: RefCell::new(vec![]),
        devices: RefCell::new(vec![]),
        settings: RefCell::new(Settings::default()),
        listeners: RefCell::new(vec![]),
        pages: RefCell::new(vec![]),
        tray: RefCell::new(None),
        tray_online: Rc::new(Cell::new(false)),
        started: Started::default(),
        stop: Arc::new(AtomicBool::new(false)),
    });

    // The pages, and the sidebar that picks one.
    for (i, (id, title, icon)) in PAGES.iter().enumerate() {
        let page: gtk::Widget = match *id {
            "sharing" => sharing::page(&ctx).upcast(),
            "viewers" => viewers::page(&ctx).upcast(),
            "devices" => devices::page(&ctx).upcast(),
            "settings" => settings::page(&ctx).upcast(),
            "signin" => admin::signin_page(&ctx).upcast(),
            "policy" => admin::policy_page(&ctx).upcast(),
            "sessions" => admin::sessions_page(&ctx).upcast(),
            "audit" => admin::audit_page(&ctx).upcast(),
            _ => admin::metrics_page(&ctx).upcast(),
        };
        stack.add_named(&page, Some(id));
        ctx.pages.borrow_mut().push((id, title));
        let b = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        b.append(&gtk::Image::from_icon_name(icon));
        b.append(&gtk::Label::new(Some(title)));
        let row = gtk::ListBoxRow::new();
        row.set_child(Some(&b));
        widgets::name(&row, title);
        sidebar.append(&row);
        if i == 3 {
            // The admin pages, apart.
            let h = gtk::Label::new(Some("Admin"));
            h.add_css_class("heading");
            h.add_css_class("dim-label");
            h.set_xalign(0.0);
            h.set_margin_start(12);
            h.set_margin_top(12);
            let r = gtk::ListBoxRow::new();
            r.set_child(Some(&h));
            r.set_selectable(false);
            r.set_activatable(false);
            sidebar.append(&r);
        }
    }
    {
        let ctx = ctx.clone();
        sidebar.connect_row_selected(move |_, row| {
            let Some(row) = row else { return };
            let mut i = row.index() as usize;
            if i == 4 {
                return;
            }
            if i > 4 {
                i -= 1; // the "Admin" heading
            }
            if let Some((id, title)) = ctx.pages.borrow().get(i).copied() {
                if ctx.page() != id {
                    ctx.stack.set_visible_child_name(id);
                }
                ctx.content_page.set_title(title);
                ctx.split.set_show_content(true);
            }
        });
    }
    {
        let ctx2 = ctx.clone();
        stack.connect_visible_child_name_notify(move |_| ctx2.changed(Change::Page));
    }
    actions(&ctx);

    // The tray icon.
    let (tray_tx, tray_rx) = async_channel::unbounded::<TrayCmd>();
    match tray::spawn(tray_tx, ctx.tray_online.clone()) {
        Ok(t) => *ctx.tray.borrow_mut() = Some(t),
        Err(e) => eprintln!("rein-remote-host-gtk: no tray icon: {e}"),
    }
    {
        let ctx = ctx.clone();
        glib::spawn_future_local(async move {
            while let Ok(cmd) = tray_rx.recv().await {
                match cmd {
                    TrayCmd::Open => ctx.present(),
                    TrayCmd::Start => ctx.set_sharing(true),
                    TrayCmd::Stop => ctx.set_sharing(false),
                    TrayCmd::NewCode => ctx.new_code(),
                    TrayCmd::Quit => ctx.quit(),
                }
            }
        });
    }

    // Closing the window: the app stays in the tray when there's one to come back from.
    {
        let ctx2 = ctx.clone();
        window.connect_close_request(move |w| {
            if ctx2.tray_online.get() {
                w.set_visible(false);
                glib::Propagation::Stop
            } else {
                ctx2.quit();
                glib::Propagation::Proceed
            }
        });
    }
    // While the tray is there, the app runs without a window.
    std::mem::forget(app.hold());

    // The connection to the host, on a thread of its own.
    let (tx, rx) = async_channel::unbounded::<Msg>();
    let opts = Options {
        home: launch::home_dir(),
        start: args.start_host,
        binary: args.binary.clone(),
        args: args.host_args.clone(),
    };
    {
        let (started, stop) = (ctx.started.clone(), ctx.stop.clone());
        std::thread::spawn(move || launch::supervise(opts, started, stop, move |m| tx.send_blocking(m).is_ok()));
    }
    {
        let ctx = ctx.clone();
        glib::spawn_future_local(async move {
            while let Ok(m) = rx.recv().await {
                ctx.on_msg(m);
            }
        });
    }

    ctx.show_page(args.page.as_deref().unwrap_or("sharing"));
    ctx.changed(Change::Link);
    if !args.hidden {
        ctx.present();
    }
    ctx
}

/// The app's actions: what the tray does, and (exported on D-Bus as GTK does, `org.gtk.Actions`)
/// what a script can ask of it.
fn actions(ctx: &Rc<Ctx>) {
    let app = &ctx.app;
    let add = |name: &str, f: ActionFn, with_string: bool| {
        let a = if with_string {
            gio::SimpleAction::new(name, Some(glib::VariantTy::STRING))
        } else {
            gio::SimpleAction::new(name, None)
        };
        let ctx = ctx.clone();
        a.connect_activate(move |_, p| f(&ctx, p));
        app.add_action(&a);
    };
    add("open", Box::new(|c, _| c.present()), false);
    add("start", Box::new(|c, _| c.set_sharing(true)), false);
    add("stop", Box::new(|c, _| c.set_sharing(false)), false);
    add("new-code", Box::new(|c, _| c.new_code()), false);
    add("quit", Box::new(|c, _| c.quit()), false);
    add(
        "show-page",
        Box::new(|c, p| {
            if let Some(p) = p.and_then(|p| p.get::<String>()) {
                c.show_page(&p);
                c.present();
            }
        }),
        true,
    );
    app.set_accels_for_action("app.quit", &["<Control>q"]);
}
