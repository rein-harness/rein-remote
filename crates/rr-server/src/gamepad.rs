//! Gamepads from the viewer, played on this machine through a virtual pad of its own: an Xbox 360
//! controller, the pad every game knows. Linux: uinput (the kernel's own virtual input devices; the
//! server needs write access to /dev/uinput). Windows: ViGEmBus, the virtual gamepad bus driver,
//! through its client library (ViGEmClient.dll, loaded when the first viewer comes, so the server
//! runs where it isn't installed). macOS has no public API for a virtual gamepad: none there.
//!
//! A pad is made when its first state comes, and goes when the session ends. They're the machine's,
//! not a window's: whatever has focus gets them, as with a real pad.

use rr_proto::gamepad::*;
use std::collections::HashMap;

/// One pad's state, as the protocol sends it (see rr-proto's `Input::Gamepad`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct State {
    pub buttons: u32,
    pub lx: i16,
    pub ly: i16,
    pub rx: i16,
    pub ry: i16,
    pub lt: u8,
    pub rt: u8,
}

trait Pad {
    fn set(&mut self, s: &State) -> Result<(), String>;
}

/// A session's virtual pads, by the viewer's pad number.
#[derive(Default)]
pub struct Pads {
    pads: HashMap<u8, Box<dyn Pad>>,
    /// Said once a session why pads can't be played here.
    said: bool,
}

/// Whether this machine can make virtual pads (said in the server's Hello), and why not.
pub fn available() -> Result<(), String> {
    imp::available()
}

impl Pads {
    pub fn set(&mut self, pad: u8, s: State) {
        if let std::collections::hash_map::Entry::Vacant(slot) = self.pads.entry(pad) {
            match imp::open(pad) {
                Ok(p) => {
                    eprintln!("rein-remote: gamepad {pad}: a virtual Xbox 360 controller ({})", imp::NAME);
                    slot.insert(p);
                }
                Err(e) => {
                    if !std::mem::replace(&mut self.said, true) {
                        eprintln!("rein-remote: the viewer's gamepad can't be played here: {e}");
                    }
                    return;
                }
            }
        }
        if let Some(p) = self.pads.get_mut(&pad) {
            if let Err(e) = p.set(&s) {
                eprintln!("rein-remote: gamepad {pad}: {e}");
                self.pads.remove(&pad);
            }
        }
    }
}

/// The Xbox 360 controller's buttons (XInput's bits) for the protocol's (the W3C standard layout).
/// ViGEmBus takes them as they are; uinput maps them on to its key codes.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn xinput_buttons(b: u32) -> u16 {
    const MAP: [(u32, u16); 15] = [
        (BUTTON_UP, 0x0001),
        (BUTTON_DOWN, 0x0002),
        (BUTTON_LEFT, 0x0004),
        (BUTTON_RIGHT, 0x0008),
        (BUTTON_START, 0x0010),
        (BUTTON_SELECT, 0x0020),
        (BUTTON_LEFT_STICK, 0x0040),
        (BUTTON_RIGHT_STICK, 0x0080),
        (BUTTON_LEFT_BUMPER, 0x0100),
        (BUTTON_RIGHT_BUMPER, 0x0200),
        (BUTTON_GUIDE, 0x0400),
        (BUTTON_SOUTH, 0x1000),
        (BUTTON_EAST, 0x2000),
        (BUTTON_WEST, 0x4000),
        (BUTTON_NORTH, 0x8000),
    ];
    MAP.iter().filter(|(p, _)| b & p != 0).fold(0, |acc, (_, x)| acc | x)
}

#[cfg(target_os = "linux")]
mod imp {
    //! uinput: a device the kernel makes when asked, as if a pad were plugged in (evdev, so SDL,
    //! Steam, Wine and games see it). It says it's Microsoft's Xbox 360 controller (USB 045e:028e),
    //! with the codes Linux's own xpad driver gives one.

    use super::{xinput_buttons, Pad, State};
    use std::ffi::c_void;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    pub const NAME: &str = "uinput";
    const PATH: &[u8] = b"/dev/uinput\0";

    const EV_SYN: u16 = 0;
    const EV_KEY: u16 = 1;
    const EV_ABS: u16 = 3;
    const ABS_X: u16 = 0;
    const ABS_Y: u16 = 1;
    const ABS_Z: u16 = 2;
    const ABS_RX: u16 = 3;
    const ABS_RY: u16 = 4;
    const ABS_RZ: u16 = 5;
    const ABS_HAT0X: u16 = 0x10;
    const ABS_HAT0Y: u16 = 0x11;
    /// XInput's bit → the key code xpad gives it (the d-pad is the hat, below).
    const KEYS: [(u16, u16); 11] = [
        (0x1000, 0x130), // A: BTN_SOUTH
        (0x2000, 0x131), // B: BTN_EAST
        (0x4000, 0x133), // X: BTN_NORTH (evdev's naming: BTN_X)
        (0x8000, 0x134), // Y: BTN_WEST (BTN_Y)
        (0x0100, 0x136), // BTN_TL
        (0x0200, 0x137), // BTN_TR
        (0x0020, 0x13a), // BTN_SELECT
        (0x0010, 0x13b), // BTN_START
        (0x0400, 0x13c), // BTN_MODE
        (0x0040, 0x13d), // BTN_THUMBL
        (0x0080, 0x13e), // BTN_THUMBR
    ];

    // The ioctls, as linux/uinput.h makes them: _IOW('U', n, size) and _IO('U', n).
    const fn iow(n: u64, size: usize) -> u64 {
        (1 << 30) | ((size as u64) << 16) | ((b'U' as u64) << 8) | n
    }
    const UI_DEV_CREATE: u64 = (b'U' as u64) << 8 | 1;
    const UI_DEV_DESTROY: u64 = (b'U' as u64) << 8 | 2;
    const UI_DEV_SETUP: u64 = iow(3, std::mem::size_of::<libc::uinput_setup>());
    const UI_ABS_SETUP: u64 = iow(4, std::mem::size_of::<libc::uinput_abs_setup>());
    const UI_SET_EVBIT: u64 = iow(100, std::mem::size_of::<libc::c_int>());
    const UI_SET_KEYBIT: u64 = iow(101, std::mem::size_of::<libc::c_int>());
    const UI_SET_ABSBIT: u64 = iow(103, std::mem::size_of::<libc::c_int>());

    fn open_uinput() -> Result<OwnedFd, String> {
        // SAFETY: a path that ends in NUL; the fd is owned from here.
        let fd = unsafe { libc::open(PATH.as_ptr().cast(), libc::O_WRONLY | libc::O_NONBLOCK | libc::O_CLOEXEC) };
        if fd < 0 {
            let e = std::io::Error::last_os_error();
            return Err(format!(
                "can't open /dev/uinput ({e}): give this user write access to it (a udev rule, or the input group), and load the module (modprobe uinput)"
            ));
        }
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    pub fn available() -> Result<(), String> {
        open_uinput().map(|_| ())
    }

    struct Uinput {
        fd: OwnedFd,
        last: Option<State>,
    }

    fn ioctl(fd: &OwnedFd, req: u64, arg: *const c_void) -> Result<(), String> {
        // SAFETY: `arg` is what `req` takes (an int by value, or a pointer to its struct).
        if unsafe { libc::ioctl(fd.as_raw_fd(), req as _, arg) } < 0 {
            return Err(format!("uinput: {}", std::io::Error::last_os_error()));
        }
        Ok(())
    }

    pub fn open(pad: u8) -> Result<Box<dyn Pad>, String> {
        let fd = open_uinput()?;
        let int = |v: u16| v as usize as *const c_void;
        for ev in [EV_KEY, EV_ABS, EV_SYN] {
            ioctl(&fd, UI_SET_EVBIT, int(ev))?;
        }
        for (_, key) in KEYS {
            ioctl(&fd, UI_SET_KEYBIT, int(key))?;
        }
        let axes = [
            (ABS_X, -32768, 32767, 16, 128),
            (ABS_Y, -32768, 32767, 16, 128),
            (ABS_RX, -32768, 32767, 16, 128),
            (ABS_RY, -32768, 32767, 16, 128),
            (ABS_Z, 0, 255, 0, 0),
            (ABS_RZ, 0, 255, 0, 0),
            (ABS_HAT0X, -1, 1, 0, 0),
            (ABS_HAT0Y, -1, 1, 0, 0),
        ];
        for (code, minimum, maximum, fuzz, flat) in axes {
            ioctl(&fd, UI_SET_ABSBIT, int(code))?;
            let setup = libc::uinput_abs_setup {
                code,
                absinfo: libc::input_absinfo {
                    value: 0,
                    minimum,
                    maximum,
                    fuzz,
                    flat,
                    resolution: 0,
                },
            };
            ioctl(&fd, UI_ABS_SETUP, &setup as *const _ as *const c_void)?;
        }
        // SAFETY: plain data, zeroes are valid.
        let mut setup: libc::uinput_setup = unsafe { std::mem::zeroed() };
        setup.id = libc::input_id {
            bustype: 0x03, // BUS_USB
            vendor: 0x045e,
            product: 0x028e,
            version: 0x0110,
        };
        let name = format!("Rein Remote gamepad {pad} (Xbox 360)");
        for (d, s) in setup.name.iter_mut().zip(name.bytes().take(libc::UINPUT_MAX_NAME_SIZE - 1)) {
            *d = s as libc::c_char;
        }
        ioctl(&fd, UI_DEV_SETUP, &setup as *const _ as *const c_void)?;
        ioctl(&fd, UI_DEV_CREATE, std::ptr::null())?;
        Ok(Box::new(Uinput { fd, last: None }))
    }

    impl Uinput {
        fn write(&self, events: &[(u16, u16, i32)]) -> Result<(), String> {
            let mut buf = Vec::with_capacity(events.len() + 1);
            for &(type_, code, value) in events.iter().chain(&[(EV_SYN, 0, 0)]) {
                buf.push(libc::input_event {
                    time: libc::timeval { tv_sec: 0, tv_usec: 0 },
                    type_,
                    code,
                    value,
                });
            }
            let bytes = std::mem::size_of_val(buf.as_slice());
            // SAFETY: the events' memory, as many bytes as they are.
            let n = unsafe { libc::write(self.fd.as_raw_fd(), buf.as_ptr().cast(), bytes) };
            if n != bytes as isize {
                return Err(format!("uinput: {}", std::io::Error::last_os_error()));
            }
            Ok(())
        }
    }

    impl Pad for Uinput {
        fn set(&mut self, s: &State) -> Result<(), String> {
            let old = self.last.unwrap_or_default();
            let (b, ob) = (xinput_buttons(s.buttons), xinput_buttons(old.buttons));
            let first = self.last.is_none();
            let mut ev = Vec::new();
            for (bit, key) in KEYS {
                if first || (b ^ ob) & bit != 0 {
                    ev.push((EV_KEY, key, (b & bit != 0) as i32));
                }
            }
            let hat = |neg: u16, pos: u16, b: u16| (b & pos != 0) as i32 - (b & neg != 0) as i32;
            let axes = [
                (ABS_X, s.lx as i32, old.lx as i32),
                (ABS_Y, s.ly as i32, old.ly as i32),
                (ABS_RX, s.rx as i32, old.rx as i32),
                (ABS_RY, s.ry as i32, old.ry as i32),
                (ABS_Z, s.lt as i32, old.lt as i32),
                (ABS_RZ, s.rt as i32, old.rt as i32),
                (ABS_HAT0X, hat(0x4, 0x8, b), hat(0x4, 0x8, ob)),
                (ABS_HAT0Y, hat(0x1, 0x2, b), hat(0x1, 0x2, ob)),
            ];
            for (code, v, o) in axes {
                if first || v != o {
                    ev.push((EV_ABS, code, v));
                }
            }
            self.last = Some(*s);
            self.write(&ev)
        }
    }

    impl Drop for Uinput {
        fn drop(&mut self) {
            let _ = ioctl(&self.fd, UI_DEV_DESTROY, std::ptr::null());
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn ioctl_numbers_are_the_kernels() {
            // From linux/uinput.h on x86_64 and aarch64.
            assert_eq!(UI_DEV_CREATE, 0x5501);
            assert_eq!(UI_DEV_SETUP, 0x405c_5503);
            assert_eq!(UI_ABS_SETUP, 0x401c_5504);
            assert_eq!(UI_SET_EVBIT, 0x4004_5564);
            assert_eq!(UI_SET_KEYBIT, 0x4004_5565);
            assert_eq!(UI_SET_ABSBIT, 0x4004_5567);
        }

        /// A pad made and read back as any program reads one (evdev). Needs /dev/uinput and the new
        /// device's /dev/input/event* (root, in CI): run with RR_UINPUT_TEST=1.
        #[test]
        fn a_virtual_pad_reads_back_through_evdev() {
            if std::env::var("RR_UINPUT_TEST").is_err() {
                eprintln!("skipped: RR_UINPUT_TEST=1 (and access to /dev/uinput and /dev/input) to run");
                return;
            }
            let mut pad = open(7).expect("a virtual pad");
            std::thread::sleep(std::time::Duration::from_millis(500));
            // Its event device, by name.
            let node = std::fs::read_dir("/sys/class/input")
                .unwrap()
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("event"))
                .find(|p| std::fs::read_to_string(p.join("device/name")).is_ok_and(|n| n.trim() == "Rein Remote gamepad 7 (Xbox 360)"))
                .expect("the device in /sys/class/input");
            let dev = format!("/dev/input/{}", node.file_name().unwrap().to_string_lossy());
            let mut f = std::fs::File::open(&dev).expect("read the event device (as root)");
            pad.set(&State {
                buttons: rr_proto::gamepad::BUTTON_SOUTH | rr_proto::gamepad::BUTTON_LEFT,
                lx: 12345,
                rt: 200,
                ..Default::default()
            })
            .unwrap();
            let mut seen = Vec::new();
            let mut buf = [0u8; std::mem::size_of::<libc::input_event>()];
            use std::io::Read;
            while seen.len() < 64 {
                f.read_exact(&mut buf).unwrap();
                let e: libc::input_event = unsafe { std::ptr::read_unaligned(buf.as_ptr().cast()) };
                if e.type_ == EV_SYN {
                    break;
                }
                seen.push((e.type_, e.code, e.value));
            }
            eprintln!("{dev}: {seen:?}");
            for want in [(EV_KEY, 0x130, 1), (EV_ABS, ABS_X, 12345), (EV_ABS, ABS_RZ, 200), (EV_ABS, ABS_HAT0X, -1)] {
                assert!(seen.contains(&want), "{want:?} in {seen:?}");
            }
        }
    }
}

#[cfg(windows)]
mod imp {
    //! ViGEmBus (github.com/nefarius/ViGEmBus): a bus driver that plugs in virtual Xbox 360 pads,
    //! which XInput, and so every game, sees as real. Driven through its client library,
    //! ViGEmClient.dll (next to rein-remote.exe or on the PATH), loaded at run time.

    use super::{xinput_buttons, Pad, State};
    use std::ffi::c_void;
    use std::sync::{Mutex, OnceLock};
    use windows::core::{s, w};
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

    pub const NAME: &str = "ViGEmBus";
    const VIGEM_ERROR_NONE: u32 = 0x2000_0000;

    #[repr(C)]
    struct XusbReport {
        buttons: u16,
        left_trigger: u8,
        right_trigger: u8,
        lx: i16,
        ly: i16,
        rx: i16,
        ry: i16,
    }

    type Client = *mut c_void;
    type Target = *mut c_void;

    struct Api {
        client: Client,
        target_x360_alloc: unsafe extern "C" fn() -> Target,
        target_add: unsafe extern "C" fn(Client, Target) -> u32,
        target_remove: unsafe extern "C" fn(Client, Target) -> u32,
        target_free: unsafe extern "C" fn(Target),
        target_x360_update: unsafe extern "C" fn(Client, Target, XusbReport) -> u32,
    }

    // The client is the bus's connection, used under the lock below.
    unsafe impl Send for Api {}
    unsafe impl Sync for Api {}

    /// The library and the bus's connection, made once (or why not).
    fn api() -> &'static Result<Mutex<Api>, String> {
        static API: OnceLock<Result<Mutex<Api>, String>> = OnceLock::new();
        API.get_or_init(|| unsafe {
            let lib = LoadLibraryW(w!("ViGEmClient.dll")).map_err(|_| {
                "ViGEmClient.dll isn't here: install ViGEmBus (github.com/nefarius/ViGEmBus/releases) and put ViGEmClient.dll next to rein-remote.exe"
                    .to_string()
            })?;
            macro_rules! f {
                ($name:literal) => {
                    std::mem::transmute(GetProcAddress(lib, s!($name)).ok_or(concat!("ViGEmClient.dll has no ", $name))?)
                };
            }
            let alloc: unsafe extern "C" fn() -> Client = f!("vigem_alloc");
            let connect: unsafe extern "C" fn(Client) -> u32 = f!("vigem_connect");
            let client = alloc();
            if client.is_null() {
                return Err("ViGEmClient: no memory".into());
            }
            let e = connect(client);
            if e != VIGEM_ERROR_NONE {
                return Err(format!(
                    "can't reach the ViGEmBus driver (error {e:#x}): install it (github.com/nefarius/ViGEmBus/releases)"
                ));
            }
            Ok(Mutex::new(Api {
                client,
                target_x360_alloc: f!("vigem_target_x360_alloc"),
                target_add: f!("vigem_target_add"),
                target_remove: f!("vigem_target_remove"),
                target_free: f!("vigem_target_free"),
                target_x360_update: f!("vigem_target_x360_update"),
            }))
        })
    }

    pub fn available() -> Result<(), String> {
        api().as_ref().map(|_| ()).map_err(Clone::clone)
    }

    struct Vigem(Target);

    // Used under the API's lock.
    unsafe impl Send for Vigem {}

    pub fn open(_pad: u8) -> Result<Box<dyn Pad>, String> {
        let api = api().as_ref().map_err(Clone::clone)?.lock().unwrap();
        unsafe {
            let t = (api.target_x360_alloc)();
            if t.is_null() {
                return Err("ViGEmClient: no memory".into());
            }
            let e = (api.target_add)(api.client, t);
            if e != VIGEM_ERROR_NONE {
                (api.target_free)(t);
                return Err(format!("ViGEmBus wouldn't plug in a pad (error {e:#x})"));
            }
            Ok(Box::new(Vigem(t)))
        }
    }

    impl Pad for Vigem {
        fn set(&mut self, s: &State) -> Result<(), String> {
            let api = api().as_ref().map_err(Clone::clone)?.lock().unwrap();
            // XInput's sticks have up as positive; the protocol's, as screens count, down.
            let flip = |v: i16| v.checked_neg().unwrap_or(i16::MAX);
            let r = XusbReport {
                buttons: xinput_buttons(s.buttons),
                left_trigger: s.lt,
                right_trigger: s.rt,
                lx: s.lx,
                ly: flip(s.ly),
                rx: s.rx,
                ry: flip(s.ry),
            };
            let e = unsafe { (api.target_x360_update)(api.client, self.0, r) };
            if e != VIGEM_ERROR_NONE {
                return Err(format!("ViGEmBus: update failed (error {e:#x})"));
            }
            Ok(())
        }
    }

    impl Drop for Vigem {
        fn drop(&mut self) {
            if let Ok(api) = api() {
                let api = api.lock().unwrap();
                unsafe {
                    (api.target_remove)(api.client, self.0);
                    (api.target_free)(self.0);
                }
            }
        }
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    use super::Pad;

    pub const NAME: &str = "";

    pub fn available() -> Result<(), String> {
        Err("macOS has no public API for a virtual gamepad".into())
    }

    pub fn open(_pad: u8) -> Result<Box<dyn Pad>, String> {
        available().map(|_| unreachable!())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_map_to_xinputs_bits() {
        assert_eq!(xinput_buttons(BUTTON_SOUTH | BUTTON_NORTH), 0x1000 | 0x8000);
        assert_eq!(xinput_buttons(BUTTON_UP | BUTTON_START | BUTTON_GUIDE), 0x0001 | 0x0010 | 0x0400);
        // The triggers' bits aren't XInput buttons (it has them as analog only), and unknown bits are ignored.
        assert_eq!(xinput_buttons(BUTTON_LEFT_TRIGGER | BUTTON_RIGHT_TRIGGER | 1 << 30), 0);
    }

    #[test]
    fn says_whether_this_machine_can_play_gamepads() {
        // In CI: no ViGEmBus on Windows runners, no /dev/uinput access on Linux ones, none on macOS;
        // each must say why in words a person can act on.
        match available() {
            Ok(()) => eprintln!("virtual gamepads: yes ({})", imp::NAME),
            Err(e) => {
                eprintln!("virtual gamepads: no: {e}");
                assert!(e.len() > 20);
            }
        }
    }
}
