//! Amiga keyboard: serial protocol via CIA-A SP with handshake.

use std::collections::VecDeque;

pub struct Keyboard {
    queue: VecDeque<u8>,
    /// Colour clocks remaining while waiting for the host handshake.
    wait: u32,
    /// Delay before the next byte may be sent.
    holdoff: u32,
    pub caps_lock: bool,
}

impl Default for Keyboard {
    fn default() -> Self {
        Self::new()
    }
}

/// Handshake timeout (~143ms) in colour clocks; the keyboard resynchronises after it.
const HANDSHAKE_TIMEOUT: u32 = 507_000;

impl Keyboard {
    pub fn new() -> Self {
        let mut k = Keyboard {
            queue: VecDeque::new(),
            wait: 0,
            holdoff: 3_500_000,
            caps_lock: false,
        };
        // Power-up key stream: "initiate" then "terminate".
        k.queue.push_back(0xfd);
        k.queue.push_back(0xfe);
        k
    }

    /// Queue a raw key code (bit 7 = key released).
    pub fn key(&mut self, code: u8, pressed: bool) {
        if code == 0x62 {
            // Caps lock toggles and only sends a key-down/up on state change.
            if !pressed {
                return;
            }
            self.caps_lock = !self.caps_lock;
            self.queue.push_back(if self.caps_lock { 0x62 } else { 0xe2 });
            return;
        }
        if self.queue.len() < 256 {
            self.queue.push_back(code | if pressed { 0 } else { 0x80 });
        }
    }

    pub fn handshake(&mut self) {
        if self.wait > 0 {
            self.wait = 0;
            self.holdoff = 200;
        }
    }

    /// Advance by `n` colour clocks. Returns a byte to shift into CIA-A SDR.
    pub fn tick(&mut self, n: u32) -> Option<u8> {
        if self.wait > 0 {
            self.wait = self.wait.saturating_sub(n);
            if self.wait > 0 {
                return None;
            }
        }
        if self.holdoff > 0 {
            self.holdoff = self.holdoff.saturating_sub(n);
            return None;
        }
        let code = self.queue.pop_front()?;
        self.wait = HANDSHAKE_TIMEOUT;
        Some(!(code.rotate_left(1)))
    }
}

/// Convert text to raw key events using the US keymap. `^X` sends Ctrl+X and
/// `\n` (or a newline) sends Return.
pub fn text_to_keys(text: &str) -> Vec<(u8, bool)> {
    const SHIFT: u8 = 0x60;
    const CTRL: u8 = 0x63;
    const LAMIGA: u8 = 0x66;
    let mut out = vec![];
    let text = text.replace("\\n", "\n");
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        // ^X is Ctrl+X; \\AX is Left Amiga+X (e.g. \\Av answers a requester's OK).
        let modifier = match c {
            '^' => Some(CTRL),
            '\\' if chars.peek() == Some(&'A') => {
                chars.next();
                Some(LAMIGA)
            }
            _ => None,
        };
        if let Some(m) = modifier {
            if let Some(n) = chars.next() {
                if let Some((k, _)) = char_key(n.to_ascii_lowercase()) {
                    out.extend([(m, true), (k, true), (k, false), (m, false)]);
                }
            }
            continue;
        }
        if let Some((k, shift)) = char_key(c) {
            if shift {
                out.push((SHIFT, true));
            }
            out.push((k, true));
            out.push((k, false));
            if shift {
                out.push((SHIFT, false));
            }
        }
    }
    out
}

fn char_key(c: char) -> Option<(u8, bool)> {
    const ROW1: &str = "`1234567890-=\\";
    const ROW1S: &str = "~!@#$%^&*()_+|";
    const ROWQ: &str = "qwertyuiop[]";
    const ROWQS: &str = "QWERTYUIOP{}";
    const ROWA: &str = "asdfghjkl;'";
    const ROWAS: &str = "ASDFGHJKL:\"";
    const ROWZ: &str = "zxcvbnm,./";
    const ROWZS: &str = "ZXCVBNM<>?";
    let find = |row: &str, base: u8| row.chars().position(|x| x == c).map(|i| base + i as u8);
    if c == ' ' {
        return Some((0x40, false));
    }
    if c == '\n' {
        return Some((0x44, false));
    }
    if let Some(k) = find(ROW1, 0x00) {
        return Some((k, false));
    }
    if let Some(k) = find(ROW1S, 0x00) {
        return Some((k, true));
    }
    for (row, rows, base) in [(ROWQ, ROWQS, 0x10), (ROWA, ROWAS, 0x20), (ROWZ, ROWZS, 0x31)] {
        if let Some(k) = find(row, base) {
            return Some((k, false));
        }
        if let Some(k) = find(rows, base) {
            return Some((k, true));
        }
    }
    None
}
