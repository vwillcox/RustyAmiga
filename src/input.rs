//! Controller mapping: which host keys and gamepad inputs work the Amiga's
//! joysticks and CD32 pad. Set up in the launcher's Controls tab and saved to
//! `~/.config/amiga/input.json`, which the emulator window reads at start.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use winit::keyboard::KeyCode;

/// A button or direction of an Amiga controller. The CD32 pad's extra buttons
/// only exist in port 2.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum JoyButton {
    Up,
    Down,
    Left,
    Right,
    /// Fire, the CD32 pad's red button.
    Fire,
    /// Second button, the CD32 pad's blue button.
    Fire2,
    Play,
    Reverse,
    Forward,
    Green,
    Yellow,
    /// The CDTV remote's mouse/joystick slider (flips on each press).
    RemoteSwitch,
}

impl JoyButton {
    pub const ALL: [JoyButton; 12] = [
        JoyButton::Up,
        JoyButton::Down,
        JoyButton::Left,
        JoyButton::Right,
        JoyButton::Fire,
        JoyButton::Fire2,
        JoyButton::Play,
        JoyButton::Reverse,
        JoyButton::Forward,
        JoyButton::Green,
        JoyButton::Yellow,
        JoyButton::RemoteSwitch,
    ];

    pub fn label(self) -> &'static str {
        match self {
            JoyButton::Up => "Up",
            JoyButton::Down => "Down",
            JoyButton::Left => "Left",
            JoyButton::Right => "Right",
            JoyButton::Fire => "Fire (CD32 red)",
            JoyButton::Fire2 => "Fire 2 (CD32 blue)",
            JoyButton::Play => "CD32 Play",
            JoyButton::Reverse => "CD32 Reverse",
            JoyButton::Forward => "CD32 Forward",
            JoyButton::Green => "CD32 Green",
            JoyButton::Yellow => "CD32 Yellow",
            JoyButton::RemoteSwitch => "CDTV remote mouse/joystick",
        }
    }

    /// A CD32 pad button (not on a plain joystick).
    pub fn cd32(self) -> bool {
        matches!(self, JoyButton::Play | JoyButton::Reverse | JoyButton::Forward | JoyButton::Green | JoyButton::Yellow)
    }

    /// Only port 2 has it: the CD32 pad's buttons and the CDTV remote's switch.
    pub fn port2_only(self) -> bool {
        self.cd32() || self == JoyButton::RemoteSwitch
    }
}

/// A gamepad button or stick direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PadInput {
    DPadUp,
    DPadDown,
    DPadLeft,
    DPadRight,
    LeftStickUp,
    LeftStickDown,
    LeftStickLeft,
    LeftStickRight,
    RightStickUp,
    RightStickDown,
    RightStickLeft,
    RightStickRight,
    South,
    East,
    West,
    North,
    LeftShoulder,
    RightShoulder,
    LeftTrigger,
    RightTrigger,
    Select,
    Start,
    Mode,
    LeftThumb,
    RightThumb,
}

impl PadInput {
    pub const ALL: [PadInput; 25] = [
        PadInput::DPadUp,
        PadInput::DPadDown,
        PadInput::DPadLeft,
        PadInput::DPadRight,
        PadInput::LeftStickUp,
        PadInput::LeftStickDown,
        PadInput::LeftStickLeft,
        PadInput::LeftStickRight,
        PadInput::RightStickUp,
        PadInput::RightStickDown,
        PadInput::RightStickLeft,
        PadInput::RightStickRight,
        PadInput::South,
        PadInput::East,
        PadInput::West,
        PadInput::North,
        PadInput::LeftShoulder,
        PadInput::RightShoulder,
        PadInput::LeftTrigger,
        PadInput::RightTrigger,
        PadInput::Select,
        PadInput::Start,
        PadInput::Mode,
        PadInput::LeftThumb,
        PadInput::RightThumb,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PadInput::DPadUp => "D-pad up",
            PadInput::DPadDown => "D-pad down",
            PadInput::DPadLeft => "D-pad left",
            PadInput::DPadRight => "D-pad right",
            PadInput::LeftStickUp => "Left stick up",
            PadInput::LeftStickDown => "Left stick down",
            PadInput::LeftStickLeft => "Left stick left",
            PadInput::LeftStickRight => "Left stick right",
            PadInput::RightStickUp => "Right stick up",
            PadInput::RightStickDown => "Right stick down",
            PadInput::RightStickLeft => "Right stick left",
            PadInput::RightStickRight => "Right stick right",
            PadInput::South => "South (A / Cross)",
            PadInput::East => "East (B / Circle)",
            PadInput::West => "West (X / Square)",
            PadInput::North => "North (Y / Triangle)",
            PadInput::LeftShoulder => "Left shoulder (LB / L1)",
            PadInput::RightShoulder => "Right shoulder (RB / R1)",
            PadInput::LeftTrigger => "Left trigger (LT / L2)",
            PadInput::RightTrigger => "Right trigger (RT / R2)",
            PadInput::Select => "Select / Back",
            PadInput::Start => "Start",
            PadInput::Mode => "Mode / Guide",
            PadInput::LeftThumb => "Left stick press (L3)",
            PadInput::RightThumb => "Right stick press (R3)",
        }
    }

    pub fn from_button(b: gilrs::Button) -> Option<PadInput> {
        use gilrs::Button::*;
        Some(match b {
            DPadUp => PadInput::DPadUp,
            DPadDown => PadInput::DPadDown,
            DPadLeft => PadInput::DPadLeft,
            DPadRight => PadInput::DPadRight,
            South => PadInput::South,
            East => PadInput::East,
            West => PadInput::West,
            North => PadInput::North,
            LeftTrigger => PadInput::LeftShoulder,
            RightTrigger => PadInput::RightShoulder,
            LeftTrigger2 => PadInput::LeftTrigger,
            RightTrigger2 => PadInput::RightTrigger,
            Select => PadInput::Select,
            Start => PadInput::Start,
            Mode => PadInput::Mode,
            LeftThumb => PadInput::LeftThumb,
            RightThumb => PadInput::RightThumb,
            _ => return None,
        })
    }

    /// The negative and positive directions of an axis (gilrs: up is positive).
    pub fn from_axis(a: gilrs::Axis) -> Option<(PadInput, PadInput)> {
        use gilrs::Axis::*;
        Some(match a {
            LeftStickX => (PadInput::LeftStickLeft, PadInput::LeftStickRight),
            LeftStickY => (PadInput::LeftStickDown, PadInput::LeftStickUp),
            RightStickX => (PadInput::RightStickLeft, PadInput::RightStickRight),
            RightStickY => (PadInput::RightStickDown, PadInput::RightStickUp),
            DPadX => (PadInput::DPadLeft, PadInput::DPadRight),
            DPadY => (PadInput::DPadDown, PadInput::DPadUp),
            _ => return None,
        })
    }
}

/// How far a stick must move to count as a direction.
pub const STICK_THRESHOLD: f32 = 0.5;

/// What a gamepad input does on the Amiga.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Action {
    /// A button or direction of the joystick (or CD32 pad) this gamepad drives.
    Joy(JoyButton),
    /// An Amiga keyboard key (raw key code).
    Key(u8),
}

impl Action {
    pub fn label(self) -> String {
        match self {
            Action::Joy(b) if b.port2_only() => b.label().to_string(),
            Action::Joy(b) => format!("Joystick {}", b.label()),
            Action::Key(k) => format!("Key {}", amiga_key_name(k)),
        }
    }
}

/// A host key working an Amiga joystick in port 1 or 2.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeyBinding {
    /// winit `KeyCode` name, e.g. "ArrowUp".
    pub key: String,
    pub port: u8,
    pub button: JoyButton,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InputMap {
    /// A joystick instead of the mouse in port 1 (two-player games).
    pub port1_joystick: bool,
    /// Keyboard joystick on from the start (Home toggles it).
    pub keys_on_at_start: bool,
    pub keys: Vec<KeyBinding>,
    /// What each gamepad input does; inputs not listed do nothing.
    pub pad: Vec<(PadInput, Action)>,
    /// The joystick port the first and second gamepad drive.
    pub pad_ports: [u8; 2],
    /// The CDTV remote starts in mouse mode.
    pub cdtv_remote_mouse: bool,
}

impl Default for InputMap {
    fn default() -> Self {
        use JoyButton::*;
        let key = |k: &str, b| KeyBinding { key: k.into(), port: 2, button: b };
        let keys = vec![
            key("ArrowUp", Up),
            key("ArrowDown", Down),
            key("ArrowLeft", Left),
            key("ArrowRight", Right),
            key("ControlRight", Fire),
            key("AltRight", Fire2),
            key("KeyP", Play),
            key("BracketLeft", Reverse),
            key("BracketRight", Forward),
            key("KeyG", Green),
            key("KeyY", Yellow),
            key("KeyM", RemoteSwitch),
        ];
        // Laid out like the CD32 pad: red, blue, green, yellow on the face
        // buttons, shoulders reverse/forward, Start play.
        let pad = vec![
            (PadInput::DPadUp, Action::Joy(Up)),
            (PadInput::DPadDown, Action::Joy(Down)),
            (PadInput::DPadLeft, Action::Joy(Left)),
            (PadInput::DPadRight, Action::Joy(Right)),
            (PadInput::LeftStickUp, Action::Joy(Up)),
            (PadInput::LeftStickDown, Action::Joy(Down)),
            (PadInput::LeftStickLeft, Action::Joy(Left)),
            (PadInput::LeftStickRight, Action::Joy(Right)),
            (PadInput::South, Action::Joy(Fire)),
            (PadInput::RightTrigger, Action::Joy(Fire)),
            (PadInput::East, Action::Joy(Fire2)),
            (PadInput::West, Action::Joy(Green)),
            (PadInput::North, Action::Joy(Yellow)),
            (PadInput::LeftShoulder, Action::Joy(Reverse)),
            (PadInput::RightShoulder, Action::Joy(Forward)),
            (PadInput::Start, Action::Joy(Play)),
            (PadInput::Select, Action::Joy(RemoteSwitch)),
        ];
        InputMap { port1_joystick: false, keys_on_at_start: false, keys, pad, pad_ports: [2, 1], cdtv_remote_mouse: false }
    }
}

impl InputMap {
    fn path() -> Option<PathBuf> {
        Some(PathBuf::from(std::env::var_os("HOME")?).join(".config/amiga/input.json"))
    }

    /// The saved mapping, or the default one.
    pub fn load() -> InputMap {
        Self::path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(p) = Self::path() else { return };
        let saved = p.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::write(&p, serde_json::to_string_pretty(self).unwrap()));
        if let Err(e) = saved {
            log::warn!("saving {}: {e}", p.display());
        }
    }

    /// The host key for a joystick button in a port, if one is set.
    pub fn key_for(&self, port: u8, button: JoyButton) -> Option<&str> {
        self.keys.iter().find(|b| b.port == port && b.button == button).map(|b| b.key.as_str())
    }

    /// Bind `key` to a button; a key does one thing only, so it leaves any
    /// other button it was on. `None` clears the button.
    pub fn set_key(&mut self, port: u8, button: JoyButton, key: Option<&str>) {
        self.keys.retain(|b| !(b.port == port && b.button == button) && Some(b.key.as_str()) != key);
        if let Some(k) = key {
            self.keys.push(KeyBinding { key: k.into(), port, button });
        }
    }

    pub fn pad_action(&self, input: PadInput) -> Option<Action> {
        self.pad.iter().find(|(i, _)| *i == input).map(|(_, a)| *a)
    }

    pub fn set_pad(&mut self, input: PadInput, action: Option<Action>) {
        self.pad.retain(|(i, _)| *i != input);
        if let Some(a) = action {
            self.pad.push((input, a));
        }
    }
}

/// Host keys that can be bound. The emulator's own hotkeys (F11, F12, Home,
/// End, Page Up/Down, Insert, Pause) are left out.
pub const HOST_KEYS: &[(KeyCode, &str)] = &[
    (KeyCode::ArrowUp, "Cursor Up"),
    (KeyCode::ArrowDown, "Cursor Down"),
    (KeyCode::ArrowLeft, "Cursor Left"),
    (KeyCode::ArrowRight, "Cursor Right"),
    (KeyCode::ControlLeft, "Left Ctrl"),
    (KeyCode::ControlRight, "Right Ctrl"),
    (KeyCode::AltLeft, "Left Alt"),
    (KeyCode::AltRight, "Right Alt / AltGr"),
    (KeyCode::ShiftLeft, "Left Shift"),
    (KeyCode::ShiftRight, "Right Shift"),
    (KeyCode::SuperLeft, "Left Super"),
    (KeyCode::SuperRight, "Right Super"),
    (KeyCode::Space, "Space"),
    (KeyCode::Enter, "Enter"),
    (KeyCode::Tab, "Tab"),
    (KeyCode::Backspace, "Backspace"),
    (KeyCode::Escape, "Esc"),
    (KeyCode::Delete, "Delete"),
    (KeyCode::CapsLock, "Caps Lock"),
    (KeyCode::KeyA, "A"),
    (KeyCode::KeyB, "B"),
    (KeyCode::KeyC, "C"),
    (KeyCode::KeyD, "D"),
    (KeyCode::KeyE, "E"),
    (KeyCode::KeyF, "F"),
    (KeyCode::KeyG, "G"),
    (KeyCode::KeyH, "H"),
    (KeyCode::KeyI, "I"),
    (KeyCode::KeyJ, "J"),
    (KeyCode::KeyK, "K"),
    (KeyCode::KeyL, "L"),
    (KeyCode::KeyM, "M"),
    (KeyCode::KeyN, "N"),
    (KeyCode::KeyO, "O"),
    (KeyCode::KeyP, "P"),
    (KeyCode::KeyQ, "Q"),
    (KeyCode::KeyR, "R"),
    (KeyCode::KeyS, "S"),
    (KeyCode::KeyT, "T"),
    (KeyCode::KeyU, "U"),
    (KeyCode::KeyV, "V"),
    (KeyCode::KeyW, "W"),
    (KeyCode::KeyX, "X"),
    (KeyCode::KeyY, "Y"),
    (KeyCode::KeyZ, "Z"),
    (KeyCode::Digit1, "1"),
    (KeyCode::Digit2, "2"),
    (KeyCode::Digit3, "3"),
    (KeyCode::Digit4, "4"),
    (KeyCode::Digit5, "5"),
    (KeyCode::Digit6, "6"),
    (KeyCode::Digit7, "7"),
    (KeyCode::Digit8, "8"),
    (KeyCode::Digit9, "9"),
    (KeyCode::Digit0, "0"),
    (KeyCode::Backquote, "`"),
    (KeyCode::Minus, "-"),
    (KeyCode::Equal, "="),
    (KeyCode::BracketLeft, "["),
    (KeyCode::BracketRight, "]"),
    (KeyCode::Backslash, "\\"),
    (KeyCode::Semicolon, ";"),
    (KeyCode::Quote, "'"),
    (KeyCode::Comma, ","),
    (KeyCode::Period, "."),
    (KeyCode::Slash, "/"),
    (KeyCode::IntlBackslash, "< > (ISO key)"),
    (KeyCode::F1, "F1"),
    (KeyCode::F2, "F2"),
    (KeyCode::F3, "F3"),
    (KeyCode::F4, "F4"),
    (KeyCode::F5, "F5"),
    (KeyCode::F6, "F6"),
    (KeyCode::F7, "F7"),
    (KeyCode::F8, "F8"),
    (KeyCode::F9, "F9"),
    (KeyCode::F10, "F10"),
    (KeyCode::Numpad0, "Keypad 0"),
    (KeyCode::Numpad1, "Keypad 1"),
    (KeyCode::Numpad2, "Keypad 2"),
    (KeyCode::Numpad3, "Keypad 3"),
    (KeyCode::Numpad4, "Keypad 4"),
    (KeyCode::Numpad5, "Keypad 5"),
    (KeyCode::Numpad6, "Keypad 6"),
    (KeyCode::Numpad7, "Keypad 7"),
    (KeyCode::Numpad8, "Keypad 8"),
    (KeyCode::Numpad9, "Keypad 9"),
    (KeyCode::NumpadEnter, "Keypad Enter"),
    (KeyCode::NumpadAdd, "Keypad +"),
    (KeyCode::NumpadSubtract, "Keypad -"),
    (KeyCode::NumpadMultiply, "Keypad *"),
    (KeyCode::NumpadDivide, "Keypad /"),
    (KeyCode::NumpadDecimal, "Keypad ."),
];

/// The name a key is saved under.
pub fn key_name(k: KeyCode) -> String {
    format!("{k:?}")
}

pub fn key_by_name(name: &str) -> Option<KeyCode> {
    HOST_KEYS.iter().map(|(k, _)| *k).find(|k| key_name(*k) == name)
}

pub fn key_label(name: &str) -> &str {
    HOST_KEYS.iter().find(|(k, _)| key_name(*k) == name).map_or(name, |(_, l)| l)
}

/// Amiga keys a gamepad input can press, most useful first.
pub const AMIGA_KEYS: &[(u8, &str)] = &[
    (0x40, "Space"),
    (0x44, "Return"),
    (0x45, "Esc"),
    (0x41, "Backspace"),
    (0x42, "Tab"),
    (0x46, "Del"),
    (0x5f, "Help"),
    (0x50, "F1"),
    (0x51, "F2"),
    (0x52, "F3"),
    (0x53, "F4"),
    (0x54, "F5"),
    (0x55, "F6"),
    (0x56, "F7"),
    (0x57, "F8"),
    (0x58, "F9"),
    (0x59, "F10"),
    (0x4c, "Cursor Up"),
    (0x4d, "Cursor Down"),
    (0x4f, "Cursor Left"),
    (0x4e, "Cursor Right"),
    (0x60, "Left Shift"),
    (0x61, "Right Shift"),
    (0x63, "Ctrl"),
    (0x64, "Left Alt"),
    (0x65, "Right Alt"),
    (0x66, "Left Amiga"),
    (0x67, "Right Amiga"),
    (0x62, "Caps Lock"),
    (0x20, "A"),
    (0x35, "B"),
    (0x33, "C"),
    (0x22, "D"),
    (0x12, "E"),
    (0x23, "F"),
    (0x24, "G"),
    (0x25, "H"),
    (0x17, "I"),
    (0x26, "J"),
    (0x27, "K"),
    (0x28, "L"),
    (0x37, "M"),
    (0x36, "N"),
    (0x18, "O"),
    (0x19, "P"),
    (0x10, "Q"),
    (0x13, "R"),
    (0x21, "S"),
    (0x14, "T"),
    (0x16, "U"),
    (0x34, "V"),
    (0x11, "W"),
    (0x32, "X"),
    (0x15, "Y"),
    (0x31, "Z"),
    (0x01, "1"),
    (0x02, "2"),
    (0x03, "3"),
    (0x04, "4"),
    (0x05, "5"),
    (0x06, "6"),
    (0x07, "7"),
    (0x08, "8"),
    (0x09, "9"),
    (0x0a, "0"),
    (0x0f, "Keypad 0"),
    (0x1d, "Keypad 1"),
    (0x1e, "Keypad 2"),
    (0x1f, "Keypad 3"),
    (0x2d, "Keypad 4"),
    (0x2e, "Keypad 5"),
    (0x2f, "Keypad 6"),
    (0x3d, "Keypad 7"),
    (0x3e, "Keypad 8"),
    (0x3f, "Keypad 9"),
    (0x43, "Keypad Enter"),
];

pub fn amiga_key_name(code: u8) -> String {
    AMIGA_KEYS.iter().find(|(c, _)| *c == code).map_or_else(|| format!("${code:02x}"), |(_, n)| n.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip() {
        for (k, _) in HOST_KEYS {
            assert_eq!(key_by_name(&key_name(*k)), Some(*k));
        }
    }

    #[test]
    fn a_key_does_one_thing() {
        let mut m = InputMap::default();
        m.set_key(1, JoyButton::Fire, Some("ControlRight"));
        assert_eq!(m.key_for(1, JoyButton::Fire), Some("ControlRight"));
        assert_eq!(m.key_for(2, JoyButton::Fire), None);
        m.set_key(1, JoyButton::Fire, None);
        assert_eq!(m.key_for(1, JoyButton::Fire), None);
    }

    #[test]
    fn saved_maps_load() {
        let m = InputMap::default();
        let back: InputMap = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(back, m);
        // Old or partial files fall back to defaults for what they lack.
        let part: InputMap = serde_json::from_str(r#"{"port1_joystick": true}"#).unwrap();
        assert!(part.port1_joystick);
        assert_eq!(part.pad, InputMap::default().pad);
    }
}
