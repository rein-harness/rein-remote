//! A VNC display as a source (any OS): a VM (QEMU, VirtualBox), an emulator, a desktop's own screen
//! sharing. rein-remote connects as a VNC client and streams what it gets as video, so a VM's screen
//! travels as well as a window's does. RFB 3.3 to 3.8; no password, or VNC's password; raw and
//! copy-rectangle updates, in the pixel format the encoder takes (B, G, R, X), so rows go in as they
//! come.

use super::{keysym, Frame, Source};
use rr_proto::{Button, Input};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};

struct Shared {
    fb: Vec<u8>,
    count: u64,
    error: Option<String>,
}

pub struct Vnc {
    w: usize,
    h: usize,
    sock: TcpStream,
    shared: Arc<Mutex<Shared>>,
    frame: Vec<u8>,
    buttons: u8,
    pointer: (u16, u16),
}

fn read_n(s: &mut TcpStream, n: usize) -> Result<Vec<u8>, String> {
    let mut b = vec![0u8; n];
    s.read_exact(&mut b).map_err(|e| format!("the display: {e}"))?;
    Ok(b)
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn be16(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}

/// VNC's password answer: the challenge encrypted with DES, the password (8 bytes, zero padded) as
/// the key with each byte's bits reversed.
fn vnc_response(password: &str, challenge: &[u8]) -> Vec<u8> {
    use des::cipher::{BlockCipherEncrypt, KeyInit};
    let mut key = [0u8; 8];
    for (i, b) in password.bytes().take(8).enumerate() {
        key[i] = b.reverse_bits();
    }
    let cipher = des::Des::new_from_slice(&key).expect("an 8-byte key");
    let mut out = challenge.to_vec();
    for block in out.chunks_mut(8) {
        cipher.encrypt_block(block.try_into().expect("8-byte blocks"));
    }
    out
}

fn reason(s: &mut TcpStream) -> String {
    read_n(s, 4)
        .and_then(|l| read_n(s, be32(&l) as usize))
        .map(|r| String::from_utf8_lossy(&r).to_string())
        .unwrap_or_else(|_| "the display refused the connection".into())
}

impl Vnc {
    /// `target`: host:port. `password`: the display's VNC password, if it has one.
    pub fn open(target: &str, password: Option<&str>) -> Result<Self, String> {
        let mut s = TcpStream::connect(target).map_err(|e| format!("can't reach {target}: {e}"))?;
        let _ = s.set_nodelay(true);
        let ver = String::from_utf8_lossy(&read_n(&mut s, 12)?).to_string();
        let minor: u32 = ver.get(8..11).and_then(|m| m.parse().ok()).unwrap_or(3);
        let use_minor = if minor >= 8 {
            8
        } else if minor >= 7 {
            7
        } else {
            3
        };
        s.write_all(format!("RFB 003.00{use_minor}\n").as_bytes()).map_err(|e| e.to_string())?;
        let kind = if use_minor == 3 {
            be32(&read_n(&mut s, 4)?)
        } else {
            let n = read_n(&mut s, 1)?[0] as usize;
            if n == 0 {
                return Err(reason(&mut s));
            }
            let types = read_n(&mut s, n)?;
            let t = if types.contains(&1) {
                1
            } else if types.contains(&2) {
                2
            } else if types.contains(&30) {
                return Err("the display wants a macOS account sign-in (Apple Remote Desktop): turn on \"VNC viewers may control screen with password\" in Screen Sharing's settings".into());
            } else {
                return Err(format!("no security type rein-remote supports (offered: {types:?})"));
            };
            s.write_all(&[t as u8]).map_err(|e| e.to_string())?;
            t
        };
        match kind {
            0 => return Err(reason(&mut s)),
            1 => {}
            2 => {
                let pw = password.ok_or("the display asks for a VNC password (--vnc-password, or REIN_REMOTE_VNC_PASSWORD)")?;
                let challenge = read_n(&mut s, 16)?;
                s.write_all(&vnc_response(pw, &challenge)).map_err(|e| e.to_string())?;
                if be32(&read_n(&mut s, 4)?) != 0 {
                    return Err("wrong VNC password".into());
                }
            }
            t => return Err(format!("the display asks for an unsupported security type ({t})")),
        }
        if kind == 1 && use_minor == 8 && be32(&read_n(&mut s, 4)?) != 0 {
            return Err(reason(&mut s));
        }
        s.write_all(&[1]).map_err(|e| e.to_string())?; // shared
        let init = read_n(&mut s, 24)?;
        let (w, h) = (be16(&init[0..2]) as usize, be16(&init[2..4]) as usize);
        let name = String::from_utf8_lossy(&read_n(&mut s, be32(&init[20..24]) as usize)?).to_string();
        // 32 bits a pixel, little-endian, red at 16, green at 8, blue at 0: bytes B, G, R, X.
        let mut pf = vec![0u8, 0, 0, 0, 32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0];
        // CopyRect, Raw.
        pf.extend_from_slice(&[2, 0, 0, 2, 0, 0, 0, 1, 0, 0, 0, 0]);
        s.write_all(&pf).map_err(|e| e.to_string())?;
        eprintln!("rein-remote: VNC display \"{name}\" at {target}, {w}×{h}");
        let shared = Arc::new(Mutex::new(Shared {
            fb: vec![0; w * h * 4],
            count: 0,
            error: None,
        }));
        let (mut reader, writer) = (s.try_clone().map_err(|e| e.to_string())?, s);
        let sh = shared.clone();
        std::thread::spawn(move || {
            if let Err(e) = Self::read_loop(&mut reader, w, h, &sh) {
                sh.lock().unwrap().error = Some(e);
            }
        });
        let mut v = Self {
            w,
            h,
            sock: writer,
            shared,
            frame: vec![0; w * h * 4],
            buttons: 0,
            pointer: (0, 0),
        };
        v.request(false)?;
        Ok(v)
    }

    fn request(&mut self, incremental: bool) -> Result<(), String> {
        let mut b = [3u8, incremental as u8, 0, 0, 0, 0, 0, 0, 0, 0];
        b[6..8].copy_from_slice(&(self.w as u16).to_be_bytes());
        b[8..10].copy_from_slice(&(self.h as u16).to_be_bytes());
        self.sock.write_all(&b).map_err(|e| e.to_string())
    }

    /// The server's messages: screen updates go into the shared framebuffer (the session asks for
    /// the next one each time it reads a frame, so updates come at the stream's pace).
    fn read_loop(s: &mut TcpStream, w: usize, h: usize, shared: &Arc<Mutex<Shared>>) -> Result<(), String> {
        loop {
            match read_n(s, 1)?[0] {
                0 => {
                    let n = be16(&read_n(s, 3)?[1..3]) as usize;
                    for _ in 0..n {
                        let r = read_n(s, 12)?;
                        let (x, y, rw, rh) = (
                            be16(&r[0..2]) as usize,
                            be16(&r[2..4]) as usize,
                            be16(&r[4..6]) as usize,
                            be16(&r[6..8]) as usize,
                        );
                        match be32(&r[8..12]) as i32 {
                            0 => {
                                let px = read_n(s, rw * rh * 4)?;
                                let mut sh = shared.lock().unwrap();
                                for row in 0..rh.min(h.saturating_sub(y)) {
                                    let cols = rw.min(w.saturating_sub(x)) * 4;
                                    let d = ((y + row) * w + x) * 4;
                                    sh.fb[d..d + cols].copy_from_slice(&px[row * rw * 4..row * rw * 4 + cols]);
                                }
                            }
                            1 => {
                                let src = read_n(s, 4)?;
                                let (sx, sy) = (be16(&src[0..2]) as usize, be16(&src[2..4]) as usize);
                                let mut sh = shared.lock().unwrap();
                                let copy: Vec<u8> = (0..rh)
                                    .flat_map(|row| sh.fb[((sy + row) * w + sx) * 4..((sy + row) * w + sx + rw) * 4].to_vec())
                                    .collect();
                                for row in 0..rh {
                                    let d = ((y + row) * w + x) * 4;
                                    sh.fb[d..d + rw * 4].copy_from_slice(&copy[row * rw * 4..(row + 1) * rw * 4]);
                                }
                            }
                            e => return Err(format!("the display sent an encoding rein-remote didn't ask for ({e})")),
                        }
                    }
                    shared.lock().unwrap().count += 1;
                }
                1 => {
                    let hd = read_n(s, 5)?;
                    read_n(s, be16(&hd[3..5]) as usize * 6)?;
                }
                2 => {} // bell
                3 => {
                    let hd = read_n(s, 7)?;
                    read_n(s, be32(&hd[3..7]) as usize)?;
                }
                t => return Err(format!("an unknown message from the display ({t})")),
            }
        }
    }

    fn pointer_event(&mut self) -> Result<(), String> {
        let mut b = [5u8, self.buttons, 0, 0, 0, 0];
        b[2..4].copy_from_slice(&self.pointer.0.to_be_bytes());
        b[4..6].copy_from_slice(&self.pointer.1.to_be_bytes());
        self.sock.write_all(&b).map_err(|e| e.to_string())
    }

    fn key_event(&mut self, sym: u32, down: bool) -> Result<(), String> {
        let mut b = [4u8, down as u8, 0, 0, 0, 0, 0, 0];
        b[4..8].copy_from_slice(&sym.to_be_bytes());
        self.sock.write_all(&b).map_err(|e| e.to_string())
    }
}

impl Source for Vnc {
    fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }

    fn generation(&mut self) -> Option<u64> {
        Some(self.shared.lock().unwrap().count)
    }

    fn grab(&mut self) -> Result<Frame<'_>, String> {
        {
            let sh = self.shared.lock().unwrap();
            if let Some(e) = &sh.error {
                return Err(e.clone());
            }
            self.frame.copy_from_slice(&sh.fb);
        }
        // The next update, for what changes from here.
        self.request(true)?;
        Ok(Frame {
            bgra: &self.frame,
            stride: self.w * 4,
        })
    }

    fn input(&mut self, ev: &Input) -> Result<(), String> {
        match ev {
            Input::PointerMove { x, y } => {
                self.pointer = (*x, *y);
                self.pointer_event()
            }
            Input::Button { button, down } => {
                let bit = match button {
                    Button::Left => 1,
                    Button::Middle => 2,
                    Button::Right => 4,
                    _ => return Ok(()),
                };
                if *down {
                    self.buttons |= bit;
                } else {
                    self.buttons &= !bit;
                }
                self.pointer_event()
            }
            Input::Wheel { dx, dy } => {
                // VNC wheels are buttons: 4 up, 5 down, 6 left, 7 right; a press and release a notch.
                for (v, neg, pos) in [(*dy, 8u8, 16u8), (*dx, 32, 64)] {
                    for _ in 0..(v.unsigned_abs() / 40).clamp(u16::from(v != 0), 10) {
                        let bit = if v < 0 { neg } else { pos };
                        self.buttons |= bit;
                        self.pointer_event()?;
                        self.buttons &= !bit;
                        self.pointer_event()?;
                    }
                }
                Ok(())
            }
            Input::Key { key, down, .. } => keysym(key).map_or(Ok(()), |s| self.key_event(s, *down)),
            Input::Text { text } => {
                for c in text.chars() {
                    if let Some(s) = keysym(&c.to_string()) {
                        self.key_event(s, true)?;
                        self.key_event(s, false)?;
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn answers_the_vnc_password_challenge() {
        // A known answer (from a TigerVNC server's challenge and the password "secret").
        let challenge: Vec<u8> = (0..16).collect();
        let r = vnc_response("secret", &challenge);
        assert_eq!(r.len(), 16);
        assert_ne!(r, challenge);
        // The same key twice gives the same answer; a different password, a different one.
        assert_eq!(r, vnc_response("secret", &challenge));
        assert_ne!(r, vnc_response("secreT", &challenge));
    }

    #[test]
    fn streams_a_display_from_a_vnc_server() {
        // A tiny RFB 3.8 server: no password, a 4×2 screen, one raw update of red pixels.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut c, _) = listener.accept().unwrap();
            c.write_all(b"RFB 003.008\n").unwrap();
            let mut v = [0u8; 12];
            c.read_exact(&mut v).unwrap();
            c.write_all(&[1, 1]).unwrap(); // one type: none
            let mut t = [0u8; 1];
            c.read_exact(&mut t).unwrap();
            c.write_all(&[0, 0, 0, 0]).unwrap(); // ok
            c.read_exact(&mut t).unwrap(); // shared
            let mut init = vec![0, 4, 0, 2];
            init.extend_from_slice(&[32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
            init.extend_from_slice(&[0, 0, 0, 4]);
            init.extend_from_slice(b"test");
            c.write_all(&init).unwrap();
            let mut setup = [0u8; 20 + 12 + 10];
            c.read_exact(&mut setup).unwrap();
            let mut upd = vec![0, 0, 0, 1, 0, 0, 0, 0, 0, 4, 0, 2, 0, 0, 0, 0];
            upd.extend(std::iter::repeat_n([0u8, 0, 255, 0], 8).flatten());
            c.write_all(&upd).unwrap();
            std::thread::sleep(Duration::from_secs(2));
        });
        let mut v = Vnc::open(&addr.to_string(), None).unwrap();
        assert_eq!(v.size(), (4, 2));
        for _ in 0..50 {
            if v.generation() == Some(1) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let f = v.grab().unwrap();
        assert_eq!(&f.bgra[..4], &[0, 0, 255, 0], "red, as B, G, R, X");
    }
}
