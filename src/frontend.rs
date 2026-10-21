//! Windowed frontend: winit + pixels for video, cpal for audio, gilrs for gamepads.

use amiga::board::Amiga;
use amiga::chipset::denise::{FB_H, FB_W};
use amiga::floppy::Disk;
use pixels::{Pixels, SurfaceTexture};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Fullscreen, Window, WindowId};

type AudioBuf = Arc<Mutex<VecDeque<f32>>>;

/// A disk image available to the disk swapper.
pub struct SwapDisk {
    pub name: String,
    pub data: Vec<u8>,
    /// File to write changes back to (None for images inside packages).
    pub path: Option<PathBuf>,
}

impl SwapDisk {
    pub fn from_path(p: &std::path::Path) -> Result<SwapDisk, String> {
        let data = std::fs::read(p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
        Ok(SwapDisk { name: p.display().to_string(), data, path: Some(p.to_path_buf()) })
    }

    pub fn to_disk(&self, write_protect: bool) -> Result<Disk, String> {
        // Re-read files so changes saved when the disk was last ejected are kept.
        let data = match &self.path {
            Some(p) => std::fs::read(p).unwrap_or_else(|_| self.data.clone()),
            None => self.data.clone(),
        };
        let mut d = Disk::from_adf(data, self.path.clone()).map_err(|e| format!("{}: {e}", self.name))?;
        d.write_protected = write_protect;
        Ok(d)
    }
}

pub struct Options {
    pub swap_disks: Vec<SwapDisk>,
    pub write_protect: bool,
}

struct App {
    amiga: Amiga,
    opts: Options,
    window: Option<Arc<Window>>,
    pixels: Option<Pixels<'static>>,
    next_frame: Instant,
    frame_time: Duration,
    warp: bool,
    grabbed: bool,
    joy_keys: bool,
    swap_index: usize,
    audio: Option<(cpal::Stream, AudioBuf, u32)>,
    gilrs: Option<gilrs::Gilrs>,
    fps_count: u32,
    fps_time: Instant,
    /// Frame counter for blinking the activity lights.
    tick: u64,
    /// Hard disk activity counter last seen, and the frame it last changed.
    hd_seen: u64,
    hd_changed: u64,
    /// Adaptive CPU speed: the lowest multiplier allowed, and the time spent
    /// emulating since the last adjustment.
    speed_floor: Option<f32>,
    busy: Duration,
}

/// Frames between adaptive CPU speed adjustments.
const SPEED_PERIOD: u64 = 25;

const HELP: &str = "\
Host keys:
  click window  capture mouse        F12        release mouse
  F11           toggle fullscreen    End        toggle warp speed
  Home          joystick on cursor keys (Right Ctrl = fire)
  PageDown      next disk in swap list into DF0
  PageUp        hard reset           Insert     Amiga HELP key
  Drag an .adf file onto the window to insert it into DF0.
The strip under the picture shows the power, floppy (green: motor on,
blinking yellow: reading/writing) and hard disk (amber: busy) lights.";

/// Map a host key to an Amiga raw key code.
fn amiga_key(k: KeyCode) -> Option<u8> {
    use KeyCode::*;
    Some(match k {
        Backquote => 0x00,
        Digit1 => 0x01,
        Digit2 => 0x02,
        Digit3 => 0x03,
        Digit4 => 0x04,
        Digit5 => 0x05,
        Digit6 => 0x06,
        Digit7 => 0x07,
        Digit8 => 0x08,
        Digit9 => 0x09,
        Digit0 => 0x0a,
        Minus => 0x0b,
        Equal => 0x0c,
        Backslash => 0x0d,
        Numpad0 => 0x0f,
        KeyQ => 0x10,
        KeyW => 0x11,
        KeyE => 0x12,
        KeyR => 0x13,
        KeyT => 0x14,
        KeyY => 0x15,
        KeyU => 0x16,
        KeyI => 0x17,
        KeyO => 0x18,
        KeyP => 0x19,
        BracketLeft => 0x1a,
        BracketRight => 0x1b,
        Numpad1 => 0x1d,
        Numpad2 => 0x1e,
        Numpad3 => 0x1f,
        KeyA => 0x20,
        KeyS => 0x21,
        KeyD => 0x22,
        KeyF => 0x23,
        KeyG => 0x24,
        KeyH => 0x25,
        KeyJ => 0x26,
        KeyK => 0x27,
        KeyL => 0x28,
        Semicolon => 0x29,
        Quote => 0x2a,
        Numpad4 => 0x2d,
        Numpad5 => 0x2e,
        Numpad6 => 0x2f,
        IntlBackslash => 0x30,
        KeyZ => 0x31,
        KeyX => 0x32,
        KeyC => 0x33,
        KeyV => 0x34,
        KeyB => 0x35,
        KeyN => 0x36,
        KeyM => 0x37,
        Comma => 0x38,
        Period => 0x39,
        Slash => 0x3a,
        NumpadDecimal => 0x3c,
        Numpad7 => 0x3d,
        Numpad8 => 0x3e,
        Numpad9 => 0x3f,
        Space => 0x40,
        Backspace => 0x41,
        Tab => 0x42,
        NumpadEnter => 0x43,
        Enter => 0x44,
        Escape => 0x45,
        Delete => 0x46,
        NumpadSubtract => 0x4a,
        ArrowUp => 0x4c,
        ArrowDown => 0x4d,
        ArrowRight => 0x4e,
        ArrowLeft => 0x4f,
        F1 => 0x50,
        F2 => 0x51,
        F3 => 0x52,
        F4 => 0x53,
        F5 => 0x54,
        F6 => 0x55,
        F7 => 0x56,
        F8 => 0x57,
        F9 => 0x58,
        F10 => 0x59,
        NumLock => 0x5a,
        ScrollLock => 0x5b,
        NumpadDivide => 0x5c,
        NumpadMultiply => 0x5d,
        NumpadAdd => 0x5e,
        Insert => 0x5f,
        ShiftLeft => 0x60,
        ShiftRight => 0x61,
        CapsLock => 0x62,
        ControlLeft => 0x63,
        AltLeft => 0x64,
        AltRight => 0x65,
        SuperLeft => 0x66,
        SuperRight | ContextMenu => 0x67,
        _ => return None,
    })
}

fn start_audio() -> Option<(cpal::Stream, AudioBuf, u32)> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let host = cpal::default_host();
    let device = host.default_output_device()?;
    let supported = device.default_output_config().ok()?;
    let rate = supported.sample_rate().0;
    let channels = supported.channels() as usize;
    let buf: AudioBuf = Arc::new(Mutex::new(VecDeque::with_capacity(48000)));
    let b2 = buf.clone();
    let err = |e| log::warn!("audio stream error: {e}");
    let config: cpal::StreamConfig = supported.config();
    let next = move |out: &mut dyn FnMut(usize, f32), frames: usize| {
        let mut q = b2.lock().unwrap();
        for f in 0..frames {
            let (l, r) = if q.len() >= 2 {
                (q.pop_front().unwrap(), q.pop_front().unwrap())
            } else {
                (0.0, 0.0)
            };
            for c in 0..channels {
                out(f * channels + c, if c % 2 == 0 { l } else { r });
            }
        }
    };
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => device
            .build_output_stream(
                &config,
                move |data: &mut [f32], _| {
                    let frames = data.len() / channels;
                    next(&mut |i, v| data[i] = v, frames);
                },
                err,
                None,
            )
            .ok()?,
        cpal::SampleFormat::I16 => device
            .build_output_stream(
                &config,
                move |data: &mut [i16], _| {
                    let frames = data.len() / channels;
                    next(&mut |i, v| data[i] = (v.clamp(-1.0, 1.0) * 32767.0) as i16, frames);
                },
                err,
                None,
            )
            .ok()?,
        f => {
            log::warn!("unsupported audio sample format {f:?}; audio disabled");
            return None;
        }
    };
    stream.play().ok()?;
    Some((stream, buf, rate))
}

impl App {
    fn set_grab(&mut self, grab: bool) {
        let Some(w) = &self.window else { return };
        if grab {
            let ok = w
                .set_cursor_grab(CursorGrabMode::Locked)
                .or_else(|_| w.set_cursor_grab(CursorGrabMode::Confined))
                .is_ok();
            w.set_cursor_visible(!ok);
            self.grabbed = ok;
        } else {
            let _ = w.set_cursor_grab(CursorGrabMode::None);
            w.set_cursor_visible(true);
            self.grabbed = false;
        }
        self.update_title();
    }

    fn update_title(&self) {
        if let Some(w) = &self.window {
            let mut t = String::from("Amiga");
            if self.grabbed {
                t.push_str(" [mouse captured: F12 releases]");
            }
            if self.joy_keys {
                t.push_str(" [cursor keys = joystick]");
            }
            if self.warp {
                t.push_str(" [warp]");
            }
            if self.speed_floor.is_some() {
                t.push_str(&format!(" [CPU x{:.0}]", self.amiga.board.cfg.cpu_speed));
            }
            w.set_title(&t);
        }
    }

    fn insert_df0(&mut self, disk: &SwapDisk) {
        match disk.to_disk(self.opts.write_protect) {
            Ok(d) => {
                self.amiga.board.drives[0].eject();
                self.amiga.board.drives[0].insert(d);
                log::info!("DF0: {}", disk.name);
            }
            Err(e) => log::error!("{e}"),
        }
    }

    fn joystick_key(&mut self, k: KeyCode, down: bool) -> bool {
        if !self.joy_keys {
            return false;
        }
        let idx = match k {
            KeyCode::ArrowUp => 0,
            KeyCode::ArrowDown => 1,
            KeyCode::ArrowLeft => 2,
            KeyCode::ArrowRight => 3,
            KeyCode::ControlRight => 4,
            _ => return false,
        };
        self.amiga.board.joy[idx] = down;
        true
    }

    fn poll_gamepads(&mut self) {
        use gilrs::{Axis, Button, EventType};
        let Some(g) = self.gilrs.as_mut() else { return };
        while let Some(ev) = g.next_event() {
            let joy = &mut self.amiga.board.joy;
            match ev.event {
                EventType::ButtonPressed(b, _) | EventType::ButtonReleased(b, _) => {
                    let down = matches!(ev.event, EventType::ButtonPressed(..));
                    match b {
                        Button::DPadUp => joy[0] = down,
                        Button::DPadDown => joy[1] = down,
                        Button::DPadLeft => joy[2] = down,
                        Button::DPadRight => joy[3] = down,
                        Button::South | Button::RightTrigger => joy[4] = down,
                        Button::East => joy[5] = down,
                        _ => {}
                    }
                }
                EventType::AxisChanged(Axis::LeftStickX, v, _) => {
                    joy[2] = v < -0.5;
                    joy[3] = v > 0.5;
                }
                EventType::AxisChanged(Axis::LeftStickY, v, _) => {
                    joy[0] = v > 0.5;
                    joy[1] = v < -0.5;
                }
                _ => {}
            }
        }
    }

    fn emulate_frame(&mut self) {
        self.poll_gamepads();
        let t = Instant::now();
        self.amiga.run_frame();
        self.busy += t.elapsed();
        let samples = self.amiga.board.take_audio();
        if let Some((_, buf, rate)) = &self.audio {
            let mut q = buf.lock().unwrap();
            if !self.warp {
                q.extend(samples);
            }
            // Keep latency bounded (~120ms).
            let max = (*rate as usize / 8) * 2;
            while q.len() > max {
                q.pop_front();
            }
        }
        self.tick += 1;
        if self.tick % SPEED_PERIOD == 0 {
            self.adapt_speed();
        }
        if let Some(n) = self.amiga.board.leds().hd {
            if n != self.hd_seen {
                self.hd_seen = n;
                self.hd_changed = self.tick;
            }
        }
        self.fps_count += 1;
        if self.fps_time.elapsed() >= Duration::from_secs(2) {
            log::debug!("{:.1} fps", self.fps_count as f64 / self.fps_time.elapsed().as_secs_f64());
            self.fps_count = 0;
            self.fps_time = Instant::now();
        }
    }

    /// Like a JIT core, run the CPU as fast as the host allows: speed up while
    /// emulation uses well under the frame time, and back off when it gets close.
    fn adapt_speed(&mut self) {
        let busy = std::mem::take(&mut self.busy);
        let Some(floor) = self.speed_floor else { return };
        if self.warp {
            return;
        }
        let load = busy.as_secs_f64() / (self.frame_time.as_secs_f64() * SPEED_PERIOD as f64);
        let speed = self.amiga.board.cfg.cpu_speed;
        let new = if load < 0.6 {
            speed * 1.25
        } else if load > 0.85 {
            speed * 0.8
        } else {
            speed
        };
        let new = new.clamp(floor, 64.0);
        if new != speed {
            self.amiga.board.set_cpu_speed(new);
            log::debug!("CPU speed x{new:.1} (load {:.0}%)", load * 100.0);
            self.update_title();
        }
    }

    fn render(&mut self) {
        let Some(px) = self.pixels.as_mut() else { return };
        let (fb, _, _) = self.amiga.board.framebuffer();
        let frame = px.frame_mut();
        for (dst, src) in frame.chunks_exact_mut(4).zip(fb.iter()) {
            dst.copy_from_slice(&src.to_le_bytes());
        }
        let leds = self.amiga.board.leds();
        // The HD light stays on briefly after each command so short bursts show.
        let hd_active = self.tick.saturating_sub(self.hd_changed) < 8 && self.hd_changed > 0;
        amiga::panel::draw(frame, FB_W, FB_H, &leds, hd_active, self.tick);
        if let Err(e) = px.render() {
            log::error!("render failed: {e}");
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Amiga")
            .with_inner_size(LogicalSize::new(FB_W as f64 * 1.5, (FB_H + amiga::panel::PANEL_H) as f64 * 1.5))
            .with_min_inner_size(LogicalSize::new(FB_W as f64 / 2.0, (FB_H + amiga::panel::PANEL_H) as f64 / 2.0));
        let window = Arc::new(el.create_window(attrs).expect("create window"));
        let size = window.inner_size();
        let surface = SurfaceTexture::new(size.width, size.height, window.clone());
        let pixels = Pixels::new(FB_W as u32, (FB_H + amiga::panel::PANEL_H) as u32, surface).expect("create pixels surface");
        self.window = Some(window);
        self.pixels = Some(pixels);
        self.next_frame = Instant::now();
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(size) => {
                if let Some(px) = self.pixels.as_mut() {
                    let _ = px.resize_surface(size.width.max(1), size.height.max(1));
                }
            }
            WindowEvent::DroppedFile(path) => match SwapDisk::from_path(&path) {
                Ok(d) => {
                    self.insert_df0(&d);
                    self.opts.swap_disks.push(d);
                    self.swap_index = self.opts.swap_disks.len() - 1;
                }
                Err(e) => log::error!("{e}"),
            },
            WindowEvent::Focused(false) => self.set_grab(false),
            WindowEvent::MouseInput { state, button, .. } => {
                let down = state == ElementState::Pressed;
                if !self.grabbed {
                    if down && button == MouseButton::Left {
                        self.set_grab(true);
                    }
                    return;
                }
                match button {
                    MouseButton::Left => self.amiga.board.mouse_left = down,
                    MouseButton::Right => self.amiga.board.mouse_right = down,
                    _ => {}
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(code) = event.physical_key else { return };
                let down = event.state == ElementState::Pressed;
                if event.repeat {
                    return;
                }
                if down {
                    match code {
                        KeyCode::F12 => return self.set_grab(false),
                        KeyCode::F11 => {
                            if let Some(w) = &self.window {
                                let fs = if w.fullscreen().is_some() { None } else { Some(Fullscreen::Borderless(None)) };
                                w.set_fullscreen(fs);
                            }
                            return;
                        }
                        KeyCode::End => {
                            self.warp = !self.warp;
                            self.next_frame = Instant::now();
                            self.update_title();
                            return;
                        }
                        KeyCode::Home => {
                            self.joy_keys = !self.joy_keys;
                            self.amiga.board.joy = [false; 6];
                            self.update_title();
                            return;
                        }
                        KeyCode::PageUp => {
                            self.amiga.hard_reset();
                            return;
                        }
                        KeyCode::PageDown => {
                            if !self.opts.swap_disks.is_empty() {
                                self.swap_index = (self.swap_index + 1) % self.opts.swap_disks.len();
                                let d = std::mem::replace(
                                    &mut self.opts.swap_disks[self.swap_index],
                                    SwapDisk { name: String::new(), data: vec![], path: None },
                                );
                                self.insert_df0(&d);
                                self.opts.swap_disks[self.swap_index] = d;
                            }
                            return;
                        }
                        _ => {}
                    }
                }
                if self.joystick_key(code, down) {
                    return;
                }
                if let Some(k) = amiga_key(code) {
                    self.amiga.board.keyboard.key(k, down);
                }
            }
            WindowEvent::RedrawRequested => {
                self.emulate_frame();
                self.render();
            }
            _ => {}
        }
    }

    fn device_event(&mut self, _el: &ActiveEventLoop, _id: DeviceId, event: DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta } = event {
            if self.grabbed {
                self.amiga.board.mouse_move(delta.0.round() as i32, delta.1.round() as i32);
            }
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        let Some(w) = &self.window else { return };
        if self.warp {
            w.request_redraw();
            el.set_control_flow(ControlFlow::Poll);
            return;
        }
        let now = Instant::now();
        if now >= self.next_frame {
            self.next_frame += self.frame_time;
            if now > self.next_frame + self.frame_time * 4 {
                self.next_frame = now + self.frame_time;
            }
            w.request_redraw();
        }
        el.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
    }
}

pub fn run(mut amiga: Amiga, opts: Options) {
    eprintln!("{HELP}");
    let audio = start_audio();
    match &audio {
        Some((_, _, rate)) => amiga.board.audio.set_sample_rate(*rate),
        None => log::warn!("no audio output available"),
    }
    let speed_floor = amiga.board.cfg.cpu_speed_adaptive.then_some(amiga.board.cfg.cpu_speed);
    let fps = if amiga.board.cfg.ntsc { 59.94 } else { 50.0 };
    let gilrs = gilrs::Gilrs::new().ok();
    let mut app = App {
        amiga,
        opts,
        window: None,
        pixels: None,
        next_frame: Instant::now(),
        frame_time: Duration::from_secs_f64(1.0 / fps),
        warp: false,
        grabbed: false,
        joy_keys: false,
        swap_index: 0,
        audio,
        gilrs,
        fps_count: 0,
        fps_time: Instant::now(),
        tick: 0,
        hd_seen: 0,
        hd_changed: 0,
        speed_floor,
        busy: Duration::ZERO,
    };
    let el = EventLoop::new().expect("event loop");
    el.run_app(&mut app).expect("run event loop");
    for d in app.amiga.board.drives.iter_mut() {
        d.eject();
    }
}
