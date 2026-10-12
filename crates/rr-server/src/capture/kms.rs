//! Linux: the screen as the GPU scans it out (KMS), for `serve --kms`. Each frame is the framebuffer on
//! the connector's CRTC, exported as a DMA-BUF and handed to the VA-API encoder as it is: the encoder
//! imports it, converts it to NV12 on the GPU and encodes it, so the CPU never touches the pixels.
//! Whatever draws the screen (X, a Wayland compositor, the console) needs nothing from us.
//!
//! Reading another program's framebuffer takes CAP_SYS_ADMIN (root): the kernel gives anyone else the
//! framebuffer's size but not its memory. Input goes through the X display when there is one
//! ($DISPLAY: XTest); without one the screen streams but takes no input.
//!
//! The kernel's ioctls are called directly (drm.h, drm_mode.h: a stable ABI), so there's no libdrm to
//! install.

use super::{Frame, Pointer, Source, Surface};
use rr_proto::Input;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

#[repr(C)]
#[derive(Default)]
struct CardRes {
    fb_id_ptr: u64,
    crtc_id_ptr: u64,
    connector_id_ptr: u64,
    encoder_id_ptr: u64,
    count_fbs: u32,
    count_crtcs: u32,
    count_connectors: u32,
    count_encoders: u32,
    min_width: u32,
    max_width: u32,
    min_height: u32,
    max_height: u32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct ModeInfo {
    clock: u32,
    hdisplay: u16,
    hsync_start: u16,
    hsync_end: u16,
    htotal: u16,
    hskew: u16,
    vdisplay: u16,
    vsync_start: u16,
    vsync_end: u16,
    vtotal: u16,
    vscan: u16,
    vrefresh: u32,
    flags: u32,
    type_: u32,
    name: [u8; 32],
}

#[repr(C)]
#[derive(Default)]
struct GetConnector {
    encoders_ptr: u64,
    modes_ptr: u64,
    props_ptr: u64,
    prop_values_ptr: u64,
    count_modes: u32,
    count_props: u32,
    count_encoders: u32,
    encoder_id: u32,
    connector_id: u32,
    connector_type: u32,
    connector_type_id: u32,
    connection: u32,
    mm_width: u32,
    mm_height: u32,
    subpixel: u32,
    pad: u32,
}

#[repr(C)]
#[derive(Default)]
struct GetEncoder {
    encoder_id: u32,
    encoder_type: u32,
    crtc_id: u32,
    possible_crtcs: u32,
    possible_clones: u32,
}

#[repr(C)]
#[derive(Default)]
struct Crtc {
    set_connectors_ptr: u64,
    count_connectors: u32,
    crtc_id: u32,
    fb_id: u32,
    x: u32,
    y: u32,
    gamma_size: u32,
    mode_valid: u32,
    mode: ModeInfo,
}

#[repr(C)]
#[derive(Default)]
struct FbCmd2 {
    fb_id: u32,
    width: u32,
    height: u32,
    pixel_format: u32,
    flags: u32,
    handles: [u32; 4],
    pitches: [u32; 4],
    offsets: [u32; 4],
    modifier: [u64; 4],
}

#[repr(C)]
#[derive(Default)]
struct PrimeHandle {
    handle: u32,
    flags: u32,
    fd: i32,
}

#[repr(C)]
#[derive(Default)]
struct GemClose {
    handle: u32,
    pad: u32,
}

/// _IOWR('d', nr, size) and _IOW: the same numbers on every Linux architecture this builds for.
const fn iowr(nr: u32, size: usize) -> u64 {
    3 << 30 | (size as u64) << 16 | (b'd' as u64) << 8 | nr as u64
}
const fn iow(nr: u32, size: usize) -> u64 {
    1 << 30 | (size as u64) << 16 | (b'd' as u64) << 8 | nr as u64
}
const GETRESOURCES: u64 = iowr(0xa0, std::mem::size_of::<CardRes>());
const GETCRTC: u64 = iowr(0xa1, std::mem::size_of::<Crtc>());
const GETENCODER: u64 = iowr(0xa6, std::mem::size_of::<GetEncoder>());
const GETCONNECTOR: u64 = iowr(0xa7, std::mem::size_of::<GetConnector>());
const GETFB2: u64 = iowr(0xce, std::mem::size_of::<FbCmd2>());
const PRIME_HANDLE_TO_FD: u64 = iowr(0x2d, std::mem::size_of::<PrimeHandle>());
const GEM_CLOSE: u64 = iow(0x09, std::mem::size_of::<GemClose>());

const DRM_MODE_FB_MODIFIERS: u32 = 1 << 1;
/// The layout is the buffer's own (set when it was made), not given: the importer asks the driver.
pub const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;
const CONNECTED: u32 = 1;

fn ioctl<T>(fd: &OwnedFd, request: u64, arg: &mut T) -> std::io::Result<()> {
    loop {
        if unsafe { libc::ioctl(fd.as_raw_fd(), request as _, arg as *mut T) } == 0 {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        if !matches!(e.raw_os_error(), Some(libc::EINTR) | Some(libc::EAGAIN)) {
            return Err(e);
        }
    }
}

/// A framebuffer as DMA-BUFs: its memory objects (each a file descriptor, closed when this drops, and
/// its size), and where each plane is in them. What the VA-API encoder imports.
pub struct DmaBuf {
    /// The DRM fourcc ("XR24": B, G, R, X in memory).
    pub format: u32,
    /// The tiling (and compression) the GPU laid it out with, or DRM_FORMAT_MOD_INVALID.
    pub modifier: u64,
    pub width: u32,
    pub height: u32,
    /// The part on this screen: x, y, width, height (one framebuffer can span screens).
    pub crop: (u32, u32, u32, u32),
    pub objects: Vec<(OwnedFd, u32)>,
    pub planes: Vec<Plane>,
}

pub struct Plane {
    pub object: usize,
    pub offset: u32,
    pub pitch: u32,
}

impl DmaBuf {
    fn try_clone(&self) -> std::io::Result<Self> {
        Ok(Self {
            objects: self
                .objects
                .iter()
                .map(|(fd, size)| Ok((fd.try_clone()?, *size)))
                .collect::<std::io::Result<_>>()?,
            planes: self
                .planes
                .iter()
                .map(|p| Plane {
                    object: p.object,
                    offset: p.offset,
                    pitch: p.pitch,
                })
                .collect(),
            ..*self
        })
    }
}

/// A screen: a connector that's lit, the CRTC that drives it, and what it shows.
struct Output {
    name: String,
    crtc: u32,
    fb: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

/// The kernel's connector names (drm_connector.c), as xrandr and /sys/class/drm show them.
fn connector_name(kind: u32, id: u32) -> String {
    const NAMES: [&str; 21] = [
        "Unknown",
        "VGA",
        "DVI-I",
        "DVI-D",
        "DVI-A",
        "Composite",
        "SVIDEO",
        "LVDS",
        "Component",
        "DIN",
        "DP",
        "HDMI-A",
        "HDMI-B",
        "TV",
        "eDP",
        "Virtual",
        "DSI",
        "DPI",
        "Writeback",
        "SPI",
        "USB",
    ];
    format!("{}-{id}", NAMES.get(kind as usize).unwrap_or(&"Unknown"))
}

/// The card's lit screens.
fn outputs(card: &OwnedFd) -> std::io::Result<Vec<Output>> {
    let mut res = CardRes::default();
    ioctl(card, GETRESOURCES, &mut res)?;
    let mut connectors = vec![0u32; res.count_connectors as usize];
    // Only the connectors (the counts of the rest set to 0: nothing else is written).
    let mut res = CardRes {
        connector_id_ptr: connectors.as_mut_ptr() as u64,
        count_connectors: connectors.len() as u32,
        ..Default::default()
    };
    ioctl(card, GETRESOURCES, &mut res)?;
    connectors.truncate(res.count_connectors as usize);
    let mut out = Vec::new();
    for id in connectors {
        // One mode's room, as libdrm's drmModeGetConnectorCurrent does: asking with none makes the
        // kernel probe the connector again, which can flicker the screen.
        let mut mode = ModeInfo::default();
        let mut c = GetConnector {
            connector_id: id,
            modes_ptr: &mut mode as *mut ModeInfo as u64,
            count_modes: 1,
            ..Default::default()
        };
        if ioctl(card, GETCONNECTOR, &mut c).is_err() || c.connection != CONNECTED || c.encoder_id == 0 {
            continue;
        }
        let mut enc = GetEncoder {
            encoder_id: c.encoder_id,
            ..Default::default()
        };
        if ioctl(card, GETENCODER, &mut enc).is_err() || enc.crtc_id == 0 {
            continue;
        }
        let mut crtc = Crtc {
            crtc_id: enc.crtc_id,
            ..Default::default()
        };
        if ioctl(card, GETCRTC, &mut crtc).is_err() || crtc.mode_valid == 0 {
            continue;
        }
        out.push(Output {
            name: connector_name(c.connector_type, c.connector_type_id),
            crtc: crtc.crtc_id,
            fb: crtc.fb_id,
            x: crtc.x,
            y: crtc.y,
            w: crtc.mode.hdisplay as u32,
            h: crtc.mode.vdisplay as u32,
        });
    }
    Ok(out)
}

/// The render node of the same GPU as a card (/dev/dri/cardN → /dev/dri/renderD…), for the encoder:
/// a tiled framebuffer can only be read by the GPU that made it.
fn render_node(card: &str) -> Option<String> {
    let name = std::path::Path::new(card).file_name()?.to_str()?;
    let dir = std::fs::read_dir(format!("/sys/class/drm/{name}/device/drm")).ok()?;
    let node = dir
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .find(|n| n.starts_with("renderD"))?;
    Some(format!("/dev/dri/{node}"))
}

fn open_card(path: &str) -> Result<OwnedFd, String> {
    let c = std::ffi::CString::new(path).map_err(|_| "a bad device path")?;
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(format!("{path}: {}", std::io::Error::last_os_error()));
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

pub struct Kms {
    card: OwnedFd,
    crtc: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    /// The last frame, given again while the screen has no framebuffer (blanked).
    last: Option<DmaBuf>,
    /// The X display, for input and the pointer, when there is one.
    x11: Option<super::x11::X11>,
    /// The GPU's render node, for the encoder.
    pub render_node: Option<String>,
    warned: bool,
}

const ROOT: &str = "reading the screen through KMS needs CAP_SYS_ADMIN: run it as root (sudo -E rein-remote serve --kms), or give the binary the capability (sudo setcap cap_sys_admin+ep rein-remote)";

impl Kms {
    /// The screen on `connector` ("HDMI-A-1", "eDP-1"; any lit one when None), on RR_KMS_DEVICE or the
    /// first card that has it.
    pub fn open(connector: Option<&str>) -> Result<Self, String> {
        let cards: Vec<String> = match std::env::var("RR_KMS_DEVICE") {
            Ok(d) => vec![d],
            Err(_) => {
                let mut c: Vec<String> = std::fs::read_dir("/dev/dri")
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|n| n.starts_with("card"))
                    .map(|n| format!("/dev/dri/{n}"))
                    .collect();
                c.sort();
                c
            }
        };
        if cards.is_empty() {
            return Err("--kms: no GPU here (/dev/dri/card*)".into());
        }
        let (mut seen, mut why) = (Vec::new(), Vec::new());
        for path in &cards {
            let card = match open_card(path) {
                Ok(c) => c,
                Err(e) => {
                    why.push(e);
                    continue;
                }
            };
            let outs = match outputs(&card) {
                Ok(o) => o,
                Err(e) => {
                    // A render-only GPU (no display engine) says it doesn't do modesetting.
                    why.push(format!("{path}: {e}"));
                    continue;
                }
            };
            let pick = outs.into_iter().find(|o| {
                seen.push(format!("{} ({}×{}, {path})", o.name, o.w, o.h));
                connector.is_none_or(|c| o.name.eq_ignore_ascii_case(c))
            });
            if let Some(o) = pick {
                let mut kms = Self {
                    card,
                    crtc: o.crtc,
                    x: o.x,
                    y: o.y,
                    w: o.w,
                    h: o.h,
                    last: None,
                    x11: None,
                    render_node: render_node(path),
                    warned: false,
                };
                // Can we read it? (Without the capability the kernel gives no memory handles.)
                if o.fb != 0 {
                    kms.export_fb(o.fb)?;
                }
                eprintln!(
                    "rein-remote: KMS {} on {path}: {}×{}{}, frames to the GPU's encoder as they are (DMA-BUF)",
                    o.name,
                    o.w,
                    o.h,
                    if (o.x, o.y) != (0, 0) {
                        format!(" at {},{} in its framebuffer", o.x, o.y)
                    } else {
                        String::new()
                    }
                );
                // Input through X when there's a display (it's the same screen).
                if std::env::var_os("DISPLAY").is_some() {
                    match super::x11::X11::open_input(None) {
                        Ok(x) => kms.x11 = Some(x),
                        Err(e) => eprintln!("rein-remote: no input ({e})"),
                    }
                } else {
                    eprintln!("rein-remote: no X display ($DISPLAY): the screen streams, but input isn't passed on");
                }
                return Ok(kms);
            }
        }
        Err(match connector {
            Some(c) if !seen.is_empty() => format!("--kms: no lit screen called {c} (there's {})", seen.join(", ")),
            _ if !why.is_empty() => format!("--kms: no lit screen ({})", why.join("; ")),
            _ => "--kms: no lit screen".into(),
        })
    }

    /// The framebuffer's memory as DMA-BUFs. The kernel makes new handles for it on each call: each is
    /// exported, then closed (else they'd pile up for as long as the device is open).
    fn export_fb(&self, fb_id: u32) -> Result<DmaBuf, String> {
        let mut fb = FbCmd2 { fb_id, ..Default::default() };
        ioctl(&self.card, GETFB2, &mut fb).map_err(|e| match e.raw_os_error() {
            Some(libc::EPERM) | Some(libc::EACCES) => ROOT.to_string(),
            _ => format!("--kms: can't read the framebuffer ({e})"),
        })?;
        let planes = fb.handles.iter().take_while(|&&h| h != 0).count();
        // Planes can share an object (one handle): each object once.
        let mut unique: Vec<u32> = Vec::new();
        for &h in &fb.handles[..planes] {
            if !unique.contains(&h) {
                unique.push(h);
            }
        }
        let close = |handles: &[u32]| {
            for &h in handles {
                let _ = ioctl(&self.card, GEM_CLOSE, &mut GemClose { handle: h, pad: 0 });
            }
        };
        if planes == 0 {
            return Err(ROOT.into());
        }
        let mut objects = Vec::new();
        for &h in &unique {
            let mut p = PrimeHandle {
                handle: h,
                flags: libc::O_CLOEXEC as u32,
                fd: -1,
            };
            if let Err(e) = ioctl(&self.card, PRIME_HANDLE_TO_FD, &mut p) {
                close(&unique);
                return Err(format!("--kms: can't export the framebuffer ({e})"));
            }
            let fd = unsafe { OwnedFd::from_raw_fd(p.fd) };
            // The object's size: where its end is.
            let size = unsafe { libc::lseek(fd.as_raw_fd(), 0, libc::SEEK_END) }.max(0) as u32;
            objects.push((fd, size));
        }
        close(&unique);
        Ok(DmaBuf {
            format: fb.pixel_format,
            modifier: if fb.flags & DRM_MODE_FB_MODIFIERS != 0 {
                fb.modifier[0]
            } else {
                DRM_FORMAT_MOD_INVALID
            },
            width: fb.width,
            height: fb.height,
            crop: (self.x, self.y, self.w, self.h),
            planes: (0..planes)
                .map(|i| Plane {
                    object: unique.iter().position(|&h| h == fb.handles[i]).unwrap_or(0),
                    offset: fb.offsets[i],
                    pitch: fb.pitches[i],
                })
                .collect(),
            objects,
        })
    }

    /// What the CRTC shows now (None: nothing, the screen is blanked).
    fn current(&mut self) -> Result<Option<DmaBuf>, String> {
        let mut crtc = Crtc {
            crtc_id: self.crtc,
            ..Default::default()
        };
        ioctl(&self.card, GETCRTC, &mut crtc).map_err(|e| format!("--kms: {e}"))?;
        if crtc.fb_id == 0 || crtc.mode_valid == 0 {
            return Ok(None);
        }
        (self.x, self.y) = (crtc.x, crtc.y);
        self.export_fb(crtc.fb_id).map(Some)
    }
}

impl Source for Kms {
    fn size(&self) -> (usize, usize) {
        (self.w as usize, self.h as usize)
    }

    fn grab(&mut self) -> Result<Frame<'_>, String> {
        Err("--kms frames only go to the GPU's encoder (VA-API), which isn't in use: see `RR_ENCODER` and the log above".into())
    }

    fn surface(&mut self) -> Option<Surface> {
        match self.current() {
            Ok(Some(b)) => {
                self.last = b.try_clone().ok();
                Some(Surface(b))
            }
            // Blanked: the last frame again (the stream goes on; the client sees it still).
            Ok(None) => self.last.as_ref().and_then(|b| b.try_clone().ok()).map(Surface),
            Err(e) => {
                if !std::mem::replace(&mut self.warned, true) {
                    eprintln!("rein-remote: {e}");
                }
                None
            }
        }
    }

    fn input(&mut self, ev: &Input) -> Result<(), String> {
        let Some(x) = &mut self.x11 else { return Ok(()) };
        // This screen's pixels, on the X screen (a framebuffer that spans screens has this one at x, y).
        match ev {
            Input::PointerMove { x: px, y: py } => x.input(&Input::PointerMove {
                x: px.saturating_add(self.x as u16),
                y: py.saturating_add(self.y as u16),
            }),
            ev => x.input(ev),
        }
    }

    fn cursor(&mut self) -> Option<Pointer> {
        let mut p = self.x11.as_mut()?.cursor()?;
        let (x, y) = (p.x as i64 - self.x as i64, p.y as i64 - self.y as i64);
        p.visible &= x >= 0 && y >= 0 && x < self.w as i64 && y < self.h as i64;
        (p.x, p.y) = (x.max(0) as u16, y.max(0) as u16);
        Some(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::size_of;

    /// The kernel's sizes and ioctl numbers (drm.h, drm_mode.h; gcc on Linux, 64-bit).
    #[cfg(target_pointer_width = "64")]
    #[test]
    fn ioctls_match_the_kernels() {
        assert_eq!(size_of::<CardRes>(), 64);
        assert_eq!(size_of::<ModeInfo>(), 68);
        assert_eq!(size_of::<Crtc>(), 104);
        assert_eq!(size_of::<GetEncoder>(), 20);
        assert_eq!(size_of::<GetConnector>(), 80);
        assert_eq!(size_of::<FbCmd2>(), 104);
        assert_eq!(std::mem::offset_of!(FbCmd2, modifier), 72);
        assert_eq!(std::mem::offset_of!(Crtc, mode), 36);
        assert_eq!(std::mem::offset_of!(GetConnector, connector_id), 48);
        assert_eq!(size_of::<PrimeHandle>(), 12);
        assert_eq!(size_of::<GemClose>(), 8);
        assert_eq!(GETRESOURCES, 0xc040_64a0);
        assert_eq!(GETCRTC, 0xc068_64a1);
        assert_eq!(GETENCODER, 0xc014_64a6);
        assert_eq!(GETCONNECTOR, 0xc050_64a7);
        assert_eq!(GETFB2, 0xc068_64ce);
        assert_eq!(PRIME_HANDLE_TO_FD, 0xc00c_642d);
        assert_eq!(GEM_CLOSE, 0x4008_6409);
        assert_eq!(libc::O_CLOEXEC, 0x80000);
    }

    #[test]
    fn connector_names_are_the_kernels() {
        assert_eq!(connector_name(11, 1), "HDMI-A-1");
        assert_eq!(connector_name(14, 1), "eDP-1");
        assert_eq!(connector_name(10, 2), "DP-2");
        assert_eq!(connector_name(99, 1), "Unknown-1");
    }

    /// No GPU (CI, a container): a clean error, not a crash.
    #[test]
    fn without_a_screen_it_says_why() {
        if let Err(e) = Kms::open(Some("no-such-connector")) {
            assert!(e.starts_with("--kms"), "{e}");
        }
    }
}
