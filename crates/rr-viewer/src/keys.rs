//! Keys as the server takes them: by name, as the web client sends them (the DOM's `KeyboardEvent.key`:
//! "a", "A", "Enter", "ArrowLeft", " "), and by position (the USB HID usage of the physical key, which
//! the web client leaves 0).

use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};

/// The key's name as the web client would send it, or None for one it has no name for.
pub fn name(key: &Key) -> Option<String> {
    Some(match key {
        Key::Character(s) => s.to_string(),
        Key::Named(n) => named(*n)?.to_string(),
        _ => return None,
    })
}

fn named(n: NamedKey) -> Option<&'static str> {
    use NamedKey::*;
    Some(match n {
        Space => " ",
        Enter => "Enter",
        Tab => "Tab",
        Backspace => "Backspace",
        Delete => "Delete",
        Escape => "Escape",
        Insert => "Insert",
        Home => "Home",
        End => "End",
        PageUp => "PageUp",
        PageDown => "PageDown",
        ArrowLeft => "ArrowLeft",
        ArrowRight => "ArrowRight",
        ArrowUp => "ArrowUp",
        ArrowDown => "ArrowDown",
        Shift => "Shift",
        Control => "Control",
        Alt => "Alt",
        AltGraph => "AltGraph",
        // The Command key on a Mac, the Windows key elsewhere: the DOM calls both Meta.
        Super | Meta => "Meta",
        CapsLock => "CapsLock",
        NumLock => "NumLock",
        ScrollLock => "ScrollLock",
        PrintScreen => "PrintScreen",
        Pause => "Pause",
        ContextMenu => "ContextMenu",
        F1 => "F1",
        F2 => "F2",
        F3 => "F3",
        F4 => "F4",
        F5 => "F5",
        F6 => "F6",
        F7 => "F7",
        F8 => "F8",
        F9 => "F9",
        F10 => "F10",
        F11 => "F11",
        F12 => "F12",
        F13 => "F13",
        F14 => "F14",
        F15 => "F15",
        F16 => "F16",
        F17 => "F17",
        F18 => "F18",
        F19 => "F19",
        F20 => "F20",
        F21 => "F21",
        F22 => "F22",
        F23 => "F23",
        F24 => "F24",
        AudioVolumeUp => "AudioVolumeUp",
        AudioVolumeDown => "AudioVolumeDown",
        AudioVolumeMute => "AudioVolumeMute",
        MediaPlayPause => "MediaPlayPause",
        MediaTrackNext => "MediaTrackNext",
        MediaTrackPrevious => "MediaTrackPrevious",
        _ => return None,
    })
}

/// The USB HID usage (keyboard page) of the key at this position, 0 when it has none here.
pub fn hid(key: PhysicalKey) -> u16 {
    use KeyCode::*;
    let PhysicalKey::Code(code) = key else { return 0 };
    let letters = [
        KeyA, KeyB, KeyC, KeyD, KeyE, KeyF, KeyG, KeyH, KeyI, KeyJ, KeyK, KeyL, KeyM, KeyN, KeyO, KeyP, KeyQ, KeyR, KeyS, KeyT, KeyU, KeyV, KeyW, KeyX, KeyY,
        KeyZ,
    ];
    if let Some(i) = letters.iter().position(|&k| k == code) {
        return 0x04 + i as u16;
    }
    // 1…9 then 0, as HID orders them.
    let digits = [Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9, Digit0];
    if let Some(i) = digits.iter().position(|&k| k == code) {
        return 0x1e + i as u16;
    }
    let f = [F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12];
    if let Some(i) = f.iter().position(|&k| k == code) {
        return 0x3a + i as u16;
    }
    let f13 = [F13, F14, F15, F16, F17, F18, F19, F20, F21, F22, F23, F24];
    if let Some(i) = f13.iter().position(|&k| k == code) {
        return 0x68 + i as u16;
    }
    match code {
        Enter => 0x28,
        Escape => 0x29,
        Backspace => 0x2a,
        Tab => 0x2b,
        Space => 0x2c,
        Minus => 0x2d,
        Equal => 0x2e,
        BracketLeft => 0x2f,
        BracketRight => 0x30,
        Backslash => 0x31,
        Semicolon => 0x33,
        Quote => 0x34,
        Backquote => 0x35,
        Comma => 0x36,
        Period => 0x37,
        Slash => 0x38,
        CapsLock => 0x39,
        PrintScreen => 0x46,
        ScrollLock => 0x47,
        Pause => 0x48,
        Insert => 0x49,
        Home => 0x4a,
        PageUp => 0x4b,
        Delete => 0x4c,
        End => 0x4d,
        PageDown => 0x4e,
        ArrowRight => 0x4f,
        ArrowLeft => 0x50,
        ArrowDown => 0x51,
        ArrowUp => 0x52,
        NumLock => 0x53,
        NumpadDivide => 0x54,
        NumpadMultiply => 0x55,
        NumpadSubtract => 0x56,
        NumpadAdd => 0x57,
        NumpadEnter => 0x58,
        Numpad1 => 0x59,
        Numpad2 => 0x5a,
        Numpad3 => 0x5b,
        Numpad4 => 0x5c,
        Numpad5 => 0x5d,
        Numpad6 => 0x5e,
        Numpad7 => 0x5f,
        Numpad8 => 0x60,
        Numpad9 => 0x61,
        Numpad0 => 0x62,
        NumpadDecimal => 0x63,
        IntlBackslash => 0x64,
        ContextMenu => 0x65,
        ControlLeft => 0xe0,
        ShiftLeft => 0xe1,
        AltLeft => 0xe2,
        SuperLeft => 0xe3,
        ControlRight => 0xe4,
        ShiftRight => 0xe5,
        AltRight => 0xe6,
        SuperRight => 0xe7,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_positions_are_the_web_clients_and_usbs() {
        assert_eq!(name(&Key::Named(NamedKey::Enter)).as_deref(), Some("Enter"));
        assert_eq!(name(&Key::Named(NamedKey::Space)).as_deref(), Some(" "));
        assert_eq!(name(&Key::Named(NamedKey::Super)).as_deref(), Some("Meta"));
        assert_eq!(name(&Key::Character("A".into())).as_deref(), Some("A"));
        assert_eq!(hid(PhysicalKey::Code(KeyCode::KeyA)), 0x04);
        assert_eq!(hid(PhysicalKey::Code(KeyCode::KeyZ)), 0x1d);
        assert_eq!(hid(PhysicalKey::Code(KeyCode::Digit0)), 0x27);
        assert_eq!(hid(PhysicalKey::Code(KeyCode::F12)), 0x45);
        assert_eq!(hid(PhysicalKey::Code(KeyCode::ArrowUp)), 0x52);
        assert_eq!(hid(PhysicalKey::Code(KeyCode::SuperRight)), 0xe7);
    }
}
