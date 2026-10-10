//! Frames from another program, on standard input: for a program that already makes its own (Rein's
//! headless browser, whose screencast frames are JPEG). Each is its length (u32, little-endian) then
//! the JPEG; rein-remote streams them as video. What the viewer does goes back on standard output,
//! a JSON line an event: {"type":"mouse","action":"down","x":…,"y":…,"button":"left"},
//! {"type":"wheel",…}, {"type":"key","action":"down","key":"a","code":""}, {"type":"text","text":…}.

use super::{Frame, Source};
use rr_proto::{Button, Input};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

struct Latest {
    jpeg: Vec<u8>,
    count: u64,
    closed: bool,
}

pub struct Pipe {
    w: usize,
    h: usize,
    latest: Arc<Mutex<Latest>>,
    decoded: u64,
    frame: Vec<u8>,
    pointer: (u16, u16),
}

impl Pipe {
    pub fn open(w: usize, h: usize) -> Self {
        let latest = Arc::new(Mutex::new(Latest {
            jpeg: vec![],
            count: 0,
            closed: false,
        }));
        let l = latest.clone();
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut len = [0u8; 4];
            while stdin.read_exact(&mut len).is_ok() {
                let mut jpeg = vec![0u8; u32::from_le_bytes(len) as usize];
                if stdin.read_exact(&mut jpeg).is_err() {
                    break;
                }
                // Only the newest matters: a frame not yet encoded is replaced, not queued.
                let mut g = l.lock().unwrap();
                g.jpeg = jpeg;
                g.count += 1;
            }
            l.lock().unwrap().closed = true;
        });
        Self {
            w: w & !1,
            h: h & !1,
            latest,
            decoded: 0,
            frame: vec![0; (w & !1) * (h & !1) * 4],
            pointer: (0, 0),
        }
    }

    fn emit(&self, json: String) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{json}");
        let _ = out.flush();
    }
}

fn esc(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            '"' => vec!['\\', '"'],
            '\\' => vec!['\\', '\\'],
            c if (c as u32) < 0x20 => format!("\\u{:04x}", c as u32).chars().collect(),
            c => vec![c],
        })
        .collect()
}

impl Source for Pipe {
    fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }

    fn generation(&mut self) -> Option<u64> {
        Some(self.latest.lock().unwrap().count)
    }

    fn grab(&mut self) -> Result<Frame<'_>, String> {
        let (jpeg, count, closed) = {
            let g = self.latest.lock().unwrap();
            (g.jpeg.clone(), g.count, g.closed)
        };
        if closed && count == self.decoded {
            return Err("the frames ended".into());
        }
        if count != self.decoded && !jpeg.is_empty() {
            self.decoded = count;
            let mut d = JpegDecoder::new_with_options(&jpeg[..], DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::BGRA));
            if let Ok(px) = d.decode() {
                let (fw, fh) = d.dimensions().unwrap_or((0, 0));
                // A frame another size than the stream's (the page was resized): its top-left part.
                let (rows, cols) = (fh.min(self.h), fw.min(self.w) * 4);
                for y in 0..rows {
                    self.frame[y * self.w * 4..y * self.w * 4 + cols].copy_from_slice(&px[y * fw * 4..y * fw * 4 + cols]);
                }
            }
        }
        Ok(Frame {
            bgra: &self.frame,
            stride: self.w * 4,
        })
    }

    fn input(&mut self, ev: &Input) -> Result<(), String> {
        let (x, y) = self.pointer;
        let line = match ev {
            Input::PointerMove { x, y } => {
                self.pointer = (*x, *y);
                format!(r#"{{"type":"mouse","action":"move","x":{x},"y":{y}}}"#)
            }
            Input::Button { button, down } => {
                let b = match button {
                    Button::Right => "right",
                    Button::Middle => "middle",
                    _ => "left",
                };
                format!(
                    r#"{{"type":"mouse","action":"{}","x":{x},"y":{y},"button":"{b}","clicks":1}}"#,
                    if *down { "down" } else { "up" }
                )
            }
            Input::Wheel { dx, dy } => format!(r#"{{"type":"wheel","x":{x},"y":{y},"dx":{dx},"dy":{dy}}}"#),
            Input::Key { key, down, .. } => format!(
                r#"{{"type":"key","action":"{}","key":"{}","code":""}}"#,
                if *down { "down" } else { "up" },
                esc(key)
            ),
            Input::Text { text } => format!(r#"{{"type":"text","text":"{}"}}"#, esc(text)),
            _ => return Ok(()),
        };
        self.emit(line);
        Ok(())
    }
}
