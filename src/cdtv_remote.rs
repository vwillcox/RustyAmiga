//! The CDTV's infrared remote in mouse mode, as the machine's input
//! controller turns it into mouse signals.
//!
//! The CDTV's game ports are driven by a 6500/1 (U75) that decodes the
//! remote's infrared codes. With the remote's slider on "mouse", its direction
//! pad and A/B buttons work the mouse port; on "joystick" they work the
//! joystick port, as the emulator's port 2 joystick does anyway.
//!
//! Timing, from the U75 firmware (Korinel's annotated disassembly) and the
//! remote's protocol: while anything is held, the remote starts a frame every
//! 60 ms. It sends a full code (9 ms mark, 4.5 ms space, 12 bits and their
//! complement, a stop mark: 42.7 ms) when what is held has changed, and a
//! repeat code (9 ms mark, 2.1 ms space, a stop mark: 11.5 ms) otherwise. For
//! a full code U75 sets the buttons (A left, B right) and steps the
//! quadrature lines four times in the direction held; for a repeat it steps
//! them eight times. 512 of its timer ticks (about 56 ms) after the last
//! frame, it lets the buttons go. So the pointer starts moving 43 ms after a
//! press, then moves 8 counts every 60 ms.

/// Microseconds.
const FRAME_PERIOD: f64 = 60_000.0;
const FULL_CODE: f64 = 42_700.0;
const REPEAT_CODE: f64 = 11_500.0;
const RELEASE: f64 = 512.0 * 108.7;

/// What is held on the remote, in the joystick's order: up, down, left,
/// right, A, B.
pub type Held = [bool; 6];

#[derive(Default)]
pub struct Remote {
    /// The slider is on "mouse".
    mouse: bool,
    now: f64,
    /// When the remote starts its next frame, while it is sending.
    next_frame: Option<f64>,
    /// The code it sent last; a repeat code repeats it.
    code: Option<Held>,
    /// A frame on its way: when U75 has it, its code, and whether it is a
    /// full code.
    in_flight: Option<(f64, Held, bool)>,
    /// U75's mouse button outputs: left, right.
    pub buttons: [bool; 2],
    release_at: Option<f64>,
}

impl Remote {
    pub fn mouse(&self) -> bool {
        self.mouse
    }

    /// Move the slider. Whatever was on its way is lost.
    pub fn set_mouse(&mut self, on: bool) {
        *self = Remote { mouse: on, ..Remote::default() };
    }

    /// Advance by `us` microseconds with `held` on the remote. Returns the
    /// mouse counts (x, y) that U75 stepped.
    pub fn tick(&mut self, us: f64, held: Held) -> (i32, i32) {
        if !self.mouse {
            return (0, 0);
        }
        self.now += us;
        let mut moved = (0, 0);
        if let Some((at, code, full)) = self.in_flight {
            if self.now >= at {
                self.in_flight = None;
                let [up, down, left, right, a, b] = code;
                let steps = if full { 4 } else { 8 };
                moved = ((right as i32 - left as i32) * steps, (down as i32 - up as i32) * steps);
                self.buttons = [a, b];
                self.release_at = Some(at + RELEASE);
            }
        }
        if self.in_flight.is_none() && self.release_at.is_some_and(|t| self.now >= t) {
            self.buttons = [false; 2];
            self.release_at = None;
        }
        let any = held.contains(&true);
        if self.next_frame.is_none() && any {
            self.next_frame = Some(self.now);
        }
        if let Some(start) = self.next_frame.filter(|&t| self.now >= t) {
            if any {
                let full = self.code != Some(held);
                self.code = Some(held);
                self.in_flight = Some((start + if full { FULL_CODE } else { REPEAT_CODE }, held, full));
                self.next_frame = Some(start + FRAME_PERIOD);
            } else {
                self.next_frame = None;
                self.code = None;
            }
        }
        moved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run for `ms` with `held`, in 64 µs steps (about a scan line), and add
    /// up the movement.
    fn run(r: &mut Remote, ms: f64, held: Held) -> (i32, i32) {
        let mut total = (0, 0);
        for _ in 0..(ms * 1000.0 / 64.0) as usize {
            let (x, y) = r.tick(64.0, held);
            total = (total.0 + x, total.1 + y);
        }
        total
    }

    const RIGHT: Held = [false, false, false, true, false, false];
    const NONE: Held = [false; 6];

    #[test]
    fn holding_a_direction_moves_four_then_eight_counts_a_frame() {
        let mut r = Remote::default();
        r.set_mouse(true);
        // The full code arrives after 42.7 ms.
        assert_eq!(run(&mut r, 42.0, RIGHT), (0, 0));
        assert_eq!(run(&mut r, 1.0, RIGHT), (4, 0));
        // Repeats start every 60 ms and take 11.5 ms each.
        assert_eq!(run(&mut r, 60.0 + 11.5 - 43.0 - 0.2, RIGHT), (0, 0));
        assert_eq!(run(&mut r, 0.4, RIGHT), (8, 0));
        // A second at the 60 ms pace: 16 more repeats.
        assert_eq!(run(&mut r, 960.0, RIGHT), (8 * 16, 0));
    }

    #[test]
    fn buttons_hold_between_frames_and_go_after_the_last() {
        let mut r = Remote::default();
        r.set_mouse(true);
        let a: Held = [false, false, false, false, true, false];
        run(&mut r, 50.0, a);
        assert_eq!(r.buttons, [true, false]);
        // Held across the gaps between repeats.
        for _ in 0..10 {
            run(&mut r, 10.0, a);
            assert_eq!(r.buttons, [true, false]);
        }
        // Released: the last repeat started at 120 ms or later and ended
        // 11.5 ms after; U75 lets go about 56 ms after that.
        run(&mut r, 110.0, NONE);
        assert_eq!(r.buttons, [false, false]);
        // Diagonals and both buttons together.
        let both: Held = [true, false, true, false, true, true];
        assert_eq!(run(&mut r, 50.0, both), (-4, -4));
        assert_eq!(r.buttons, [true, true]);
    }

    #[test]
    fn joystick_mode_leaves_the_mouse_alone() {
        let mut r = Remote::default();
        assert_eq!(run(&mut r, 500.0, RIGHT), (0, 0));
        assert_eq!(r.buttons, [false, false]);
    }
}
