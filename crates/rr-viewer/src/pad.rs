//! Gamepads (gilrs: XInput and Windows.Gaming.Input on Windows, IOKit on macOS, evdev on Linux),
//! sent as the protocol's gamepad state: the whole pad each time any of it changes, its buttons in the
//! W3C standard layout (rr-proto's `gamepad` module), sticks and analog triggers. The server plays
//! them through a virtual pad of its own (uinput on Linux, ViGEmBus on Windows).

use rr_proto::gamepad::*;
use rr_proto::Input;
use std::time::Duration;

/// Watches the gamepads on a thread of its own for as long as the viewer runs, sending each change
/// through `send`. Nothing happens when there are none, or gilrs can't start (it says why, once).
pub fn watch(send: impl Fn(Input) + Send + 'static) {
    std::thread::spawn(move || {
        // gilrs is made on the thread that uses it (on some systems it can't move between threads).
        let mut gilrs = match gilrs::Gilrs::new() {
            Ok(g) => g,
            Err(e) => {
                eprintln!("rein-remote-viewer: no gamepads ({e})");
                return;
            }
        };
        // Pads by gilrs's id, each with the number the server knows it by and what was last sent.
        let mut pads: Vec<(gilrs::GamepadId, u8, Option<Input>)> = Vec::new();
        for (id, g) in gilrs.gamepads() {
            eprintln!("rein-remote-viewer: gamepad {}: {}", pads.len(), g.name());
            pads.push((id, pads.len() as u8, None));
        }
        loop {
            let Some(ev) = gilrs.next_event_blocking(Some(Duration::from_millis(500))) else {
                continue;
            };
            let id = ev.id;
            match ev.event {
                gilrs::EventType::Connected => {
                    if !pads.iter().any(|p| p.0 == id) {
                        // The lowest number not in use.
                        let n = (0..=u8::MAX).find(|n| !pads.iter().any(|p| p.1 == *n)).unwrap_or(0);
                        eprintln!("rein-remote-viewer: gamepad {n}: {}", gilrs.gamepad(id).name());
                        pads.push((id, n, None));
                    }
                    continue;
                }
                gilrs::EventType::Disconnected => {
                    // Once more at rest, so nothing stays held on the server.
                    if let Some(i) = pads.iter().position(|p| p.0 == id) {
                        let (_, n, _) = pads.remove(i);
                        send(rest(n));
                    }
                    continue;
                }
                _ => {}
            }
            let Some(p) = pads.iter_mut().find(|p| p.0 == id) else { continue };
            let now = state(p.1, &gilrs.gamepad(id));
            if p.2.as_ref() != Some(&now) {
                send(now.clone());
                p.2 = Some(now);
            }
        }
    });
}

fn rest(pad: u8) -> Input {
    Input::Gamepad {
        pad,
        buttons: 0,
        lx: 0,
        ly: 0,
        rx: 0,
        ry: 0,
        lt: 0,
        rt: 0,
    }
}

/// The pad's whole state as the protocol has it.
fn state(pad: u8, g: &gilrs::Gamepad) -> Input {
    use gilrs::{Axis, Button};
    const BUTTONS: [(Button, u32); 17] = [
        (Button::South, BUTTON_SOUTH),
        (Button::East, BUTTON_EAST),
        (Button::West, BUTTON_WEST),
        (Button::North, BUTTON_NORTH),
        (Button::LeftTrigger, BUTTON_LEFT_BUMPER),
        (Button::RightTrigger, BUTTON_RIGHT_BUMPER),
        (Button::LeftTrigger2, BUTTON_LEFT_TRIGGER),
        (Button::RightTrigger2, BUTTON_RIGHT_TRIGGER),
        (Button::Select, BUTTON_SELECT),
        (Button::Start, BUTTON_START),
        (Button::LeftThumb, BUTTON_LEFT_STICK),
        (Button::RightThumb, BUTTON_RIGHT_STICK),
        (Button::DPadUp, BUTTON_UP),
        (Button::DPadDown, BUTTON_DOWN),
        (Button::DPadLeft, BUTTON_LEFT),
        (Button::DPadRight, BUTTON_RIGHT),
        (Button::Mode, BUTTON_GUIDE),
    ];
    let trigger = |b: Button| g.button_data(b).map_or(0.0, |d| d.value());
    let (lt, rt) = (trigger(Button::LeftTrigger2), trigger(Button::RightTrigger2));
    let mut buttons = BUTTONS.iter().filter(|(b, _)| g.is_pressed(*b)).fold(0, |acc, (_, bit)| acc | bit);
    // A pad whose triggers are only analog: pressed past half way.
    if lt > 0.5 {
        buttons |= BUTTON_LEFT_TRIGGER;
    }
    if rt > 0.5 {
        buttons |= BUTTON_RIGHT_TRIGGER;
    }
    Input::Gamepad {
        pad,
        buttons,
        lx: stick(g.value(Axis::LeftStickX)),
        // gilrs has up as positive; the protocol has up as negative (as screens count).
        ly: stick(-g.value(Axis::LeftStickY)),
        rx: stick(g.value(Axis::RightStickX)),
        ry: stick(-g.value(Axis::RightStickY)),
        lt: (lt.clamp(0.0, 1.0) * 255.0).round() as u8,
        rt: (rt.clamp(0.0, 1.0) * 255.0).round() as u8,
    }
}

/// −1…1 as −32768…32767.
fn stick(v: f32) -> i16 {
    if v < 0.0 {
        (v.max(-1.0) * 32768.0).round() as i16
    } else {
        (v.min(1.0) * 32767.0).round() as i16
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn sticks_reach_both_ends() {
        assert_eq!(super::stick(-1.0), i16::MIN);
        assert_eq!(super::stick(1.0), i16::MAX);
        assert_eq!(super::stick(0.0), 0);
        assert_eq!(super::stick(-2.0), i16::MIN);
    }
}
