//! UAE's virtual graphics card for Picasso96 ("uaegfx").
//!
//! The card is a Zorro III board (manufacturer 2011, product 96) whose space is
//! plain video RAM. Its driver calls the emulator with `jsr $F0FF60` (UAE's
//! uaelib entry point) and the function number in the long at 4(a7); the result
//! goes in d0. Functions follow the Picasso96 BoardInfo calling conventions and
//! WinUAE's picasso96 module.
//!
//! Like WinUAE, the emulator supplies the driver itself: a ROM at $F00000 holds a
//! resident that builds `uaegfx.card` in memory at boot (Amiga Forever's RTG
//! monitor opens it by that name), and InitCard points the BoardInfo functions at
//! trap stubs in that ROM. The old disk-based uaegfx.card 0.9 also works through
//! the same traps. The blitter functions draw straight into VRAM (see `blit`).

mod blit;

use crate::board::Board;
use crate::bus::Bus;
use crate::cpu::Cpu;

/// Where the driver calls the emulator.
pub const TRAP_PC: u32 = 0xf0ff60;
/// The emulator's ROM with the card driver.
const ROM_BASE: u32 = 0xf00000;
const ROM_SIZE: usize = 0x10000;
pub const MANUFACTURER: u16 = 2011;
pub const PRODUCT: u8 = 96;

// BoardInfo offsets (Picasso96's boardinfo.h).
const BI_MEMORY_BASE: u32 = 4;
const BI_MEMORY_SIZE: u32 = 12;
const BI_BOARD_NAME: u32 = 16;
const BI_RESOLUTIONS_LIST: u32 = 158;
const BI_BOARD_TYPE: u32 = 170;
const BI_PALETTE_CHIP_TYPE: u32 = 174;
const BI_GRAPHICS_CONTROLLER_TYPE: u32 = 178;
const BI_BITS_PER_CANNON: u32 = 184;
const BI_FLAGS: u32 = 186;
const BI_RGB_FORMATS: u32 = 200;
const BI_MAX_HOR_VALUE: u32 = 202;
const BI_MAX_VER_VALUE: u32 = 212;
const BI_MAX_HOR_RESOLUTION: u32 = 222;
const BI_MAX_VER_RESOLUTION: u32 = 232;
const BI_MEMORY_CLOCK: u32 = 250;
const BI_PIXEL_CLOCK_COUNT: u32 = 254;
const BI_MODE_INFO: u32 = 558;
const BI_XOFFSET: u32 = 566;
const BI_YOFFSET: u32 = 568;
const BI_BORDER: u32 = 572;
const BI_CLUT: u32 = 578;
const BI_BITMAP_EXTRA: u32 = BI_CLUT + 3 * 256 + 8;
const BI_MOUSE_X: u32 = 1382;
const BI_MOUSE_Y: u32 = 1384;
const BI_MOUSE_WIDTH: u32 = 1386;
const BI_MOUSE_HEIGHT: u32 = 1387;
const BI_MOUSE_IMAGE: u32 = 1390;

// BoardInfo function slots.
const BI_SET_SWITCH: u32 = 282;
const BI_SET_COLOR_ARRAY: u32 = 286;
const BI_SET_DAC: u32 = 290;
const BI_SET_GC: u32 = 294;
const BI_SET_PANNING: u32 = 298;
const BI_CALCULATE_BYTES_PER_ROW: u32 = 302;
const BI_CALCULATE_MEMORY: u32 = 306;
const BI_GET_COMPATIBLE_FORMATS: u32 = 310;
const BI_SET_DISPLAY: u32 = 314;
const BI_RESOLVE_PIXEL_CLOCK: u32 = 318;
const BI_GET_PIXEL_CLOCK: u32 = 322;
const BI_SET_CLOCK: u32 = 326;
const BI_SET_MEMORY_MODE: u32 = 330;
const BI_SET_WRITE_MASK: u32 = 334;
const BI_SET_CLEAR_MASK: u32 = 338;
const BI_SET_READ_PLANE: u32 = 342;
const BI_WAIT_VERTICAL_SYNC: u32 = 346;
const BI_WAIT_BLITTER: u32 = 354;
const BI_SCROLL_PLANAR: u32 = 358;
const BI_UPDATE_PLANAR: u32 = 366;
const BI_BLIT_PLANAR2CHUNKY: u32 = 374;
const BI_FILL_RECT: u32 = 382;
const BI_INVERT_RECT: u32 = 390;
const BI_BLIT_RECT: u32 = 398;
const BI_BLIT_TEMPLATE: u32 = 406;
const BI_BLIT_PATTERN: u32 = 414;
const BI_DRAW_LINE: u32 = 422;
const BI_BLIT_RECT_NO_MASK_COMPLETE: u32 = 430;
const BI_BLIT_PLANAR2DIRECT: u32 = 438;
const BI_BLIT_RECT_TRANSPARENT: u32 = 478;
const BI_SET_SPRITE: u32 = 518;
const BI_SET_SPRITE_POSITION: u32 = 522;
const BI_SET_SPRITE_IMAGE: u32 = 526;
const BI_SET_SPRITE_COLOR: u32 = 530;

/// Functions that go to the emulator (trap number, BoardInfo slot).
const CALLS: [(u32, u32); 11] = [
    (18, BI_SET_SWITCH),
    (19, BI_SET_COLOR_ARRAY),
    (20, BI_SET_DAC),
    (21, BI_SET_GC),
    (22, BI_SET_PANNING),
    (23, BI_CALCULATE_BYTES_PER_ROW),
    (26, BI_SET_DISPLAY),
    (36, BI_SET_SPRITE),
    (37, BI_SET_SPRITE_POSITION),
    (38, BI_SET_SPRITE_IMAGE),
    (39, BI_SET_SPRITE_COLOR),
];
/// Blitter functions: the emulator's, else rtg.library's own (the slot after).
const BLITS: [(u32, u32); 9] = [
    (17, BI_FILL_RECT),
    (24, BI_BLIT_PLANAR2CHUNKY),
    (25, BI_BLIT_RECT),
    (27, BI_BLIT_TEMPLATE),
    (28, BI_BLIT_RECT_NO_MASK_COMPLETE),
    (30, BI_BLIT_PATTERN),
    (31, BI_INVERT_RECT),
    (32, BI_BLIT_PLANAR2DIRECT),
    (40, BI_BLIT_RECT_TRANSPARENT),
];
/// Functions that always use rtg.library's own versions.
const DEFAULTS: [u32; 3] = [BI_SCROLL_PLANAR, BI_UPDATE_PLANAR, BI_DRAW_LINE];
/// Functions that do nothing on this card.
const NOTHING: [u32; 7] = [BI_SET_CLOCK, BI_SET_MEMORY_MODE, BI_SET_WRITE_MASK, BI_SET_CLEAR_MASK, BI_SET_READ_PLANE, BI_WAIT_VERTICAL_SYNC, BI_WAIT_BLITTER];

const BME_WIDTH: u32 = 40;
const BME_HEIGHT: u32 = 42;

const MI_ACTIVE: u32 = 16;
const MI_WIDTH: u32 = 18;
const MI_HEIGHT: u32 = 20;
const MI_DEPTH: u32 = 22;
const MI_HOR_TOTAL: u32 = 24;
const MI_HOR_BLANK_SIZE: u32 = 26;
const MI_HOR_SYNC_START: u32 = 28;
const MI_HOR_SYNC_SIZE: u32 = 30;
const MI_VER_TOTAL: u32 = 34;
const MI_VER_BLANK_SIZE: u32 = 36;
const MI_VER_SYNC_START: u32 = 38;
const MI_VER_SYNC_SIZE: u32 = 40;
const MI_NUMERATOR: u32 = 42;
const MI_DENOMINATOR: u32 = 43;
const MI_PIXEL_CLOCK: u32 = 44;
const MODE_INFO_SIZE: u32 = 48;

const LR_P96ID: u32 = 14;
const LR_NAME: u32 = 20;
const LR_DISPLAY_ID: u32 = 42;
const LR_WIDTH: u32 = 46;
const LR_HEIGHT: u32 = 48;
const LR_FLAGS: u32 = 50;
const LR_MODES: u32 = 52;
const LR_BOARD_INFO: u32 = 72;
const LIB_RESOLUTION_SIZE: u32 = 80;
const P96F_PUBLIC: u16 = 2;

// BoardInfo flags.
const BIF_HARDWARESPRITE: u32 = 1 << 0;
const BIF_NOMEMORYMODEMIX: u32 = 1 << 1;
const BIF_BLITTER: u32 = 1 << 15;
const BIF_HIRESSPRITE: u32 = 1 << 16;
const BIF_BIGSPRITE: u32 = 1 << 17;
const BIF_INDISPLAYCHAIN: u32 = 1 << 20;
const BIF_GRANTDIRECTACCESS: u32 = 1 << 26;

const BT_UAEGFX: u32 = 14;

// RGB formats (RGBFTYPE).
const RGBFB_CLUT: u32 = 1;
const RGBFB_R8G8B8: u32 = 2;
const RGBFB_B8G8R8: u32 = 3;
const RGBFB_R5G6B5PC: u32 = 4;
const RGBFB_R5G5B5PC: u32 = 5;
const RGBFB_A8R8G8B8: u32 = 6;
const RGBFB_A8B8G8R8: u32 = 7;
const RGBFB_R8G8B8A8: u32 = 8;
const RGBFB_B8G8R8A8: u32 = 9;
const RGBFB_R5G6B5: u32 = 10;
const RGBFB_R5G5B5: u32 = 11;
const RGBFB_B5G6R5PC: u32 = 12;
const RGBFB_B5G5R5PC: u32 = 13;

/// The formats offered, one per depth (as WinUAE's defaults): 8-bit CLUT,
/// 16-bit R5G6B5PC and 32-bit B8G8R8A8.
const FORMATS: u32 = 1 << RGBFB_CLUT | 1 << RGBFB_R5G6B5PC | 1 << RGBFB_B8G8R8A8;
const DEPTHS: [u8; 3] = [8, 16, 32];

/// Resolutions offered, with their indices in UAE's display ID table (ID =
/// $50001000 + index << 16), so saved ScreenMode prefs pick the same modes.
const MODES: [(u16, u16, u32); 12] = [
    (320, 240, 1),
    (640, 400, 2),
    (640, 480, 3),
    (640, 512, 10),
    (800, 600, 4),
    (1024, 768, 5),
    (1152, 864, 6),
    (1280, 720, 142),
    (1280, 1024, 7),
    (1440, 900, 148),
    (1600, 1200, 150),
    (1920, 1080, 152),
];

const HOR_BLANK: u32 = 128;
const VER_BLANK: u32 = 44;

/// Bytes per pixel of an RGB format (0 for planar or unknown formats).
fn bytes_per_pixel(f: u32) -> u32 {
    match f {
        RGBFB_CLUT => 1,
        RGBFB_R8G8B8 | RGBFB_B8G8R8 => 3,
        RGBFB_A8R8G8B8 | RGBFB_A8B8G8R8 | RGBFB_R8G8B8A8 | RGBFB_B8G8R8A8 => 4,
        RGBFB_R5G6B5PC | RGBFB_R5G5B5PC | RGBFB_R5G6B5 | RGBFB_R5G5B5 | RGBFB_B5G6R5PC | RGBFB_B5G5R5PC => 2,
        _ => 0,
    }
}

fn format_for_depth(depth: u8) -> u32 {
    match depth {
        8 => RGBFB_CLUT,
        16 => RGBFB_R5G6B5PC,
        _ => RGBFB_B8G8R8A8,
    }
}

/// A minimal 68k code emitter for the ROM.
struct Rom {
    buf: Vec<u8>,
}

impl Rom {
    fn here(&self) -> u32 {
        ROM_BASE + self.buf.len() as u32
    }
    fn w(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    fn l(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    fn str(&mut self, s: &str) -> u32 {
        let at = self.here();
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
        if self.buf.len() % 2 == 1 {
            self.buf.push(0);
        }
        at
    }
    /// `move.l #n,-(sp); jsr TRAP_PC; addq.l #4,sp`
    fn trap(&mut self, n: u32) {
        self.w(0x2f3c);
        self.l(n);
        self.w(0x4eb9);
        self.l(TRAP_PC);
        self.w(0x588f);
    }
}

/// Where the ROM's routines are.
#[derive(Clone, Copy)]
struct Layout {
    rts: u32,
    /// The stub for each trap number: `trap; rts`.
    call: [u32; 41],
    /// Blitter stubs: trap, and if it returns 0, jump to rtg.library's default.
    blit: [u32; 41],
    /// rtg.library's default for each slot in DEFAULTS.
    defaults: [u32; 3],
    resolve_pixel_clock: u32,
    get_pixel_clock: u32,
    calculate_memory: u32,
    get_compatible_formats: u32,
    board_name: u32,
}

const LIBRARY_VERSION: u16 = 3;
const LIBRARY_REVISION: u16 = 5;

fn build() -> (Vec<u8>, Layout) {
    let mut rom = Rom { buf: vec![0; 0x10] };
    let mut lay = Layout {
        rts: 0,
        call: [0; 41],
        blit: [0; 41],
        defaults: [0; 3],
        resolve_pixel_clock: 0,
        get_pixel_clock: 0,
        calculate_memory: 0,
        get_compatible_formats: 0,
        board_name: 0,
    };
    let name = rom.str("uaegfx.card");
    let id = rom.str("UAE Graphics Card 3.5");
    lay.board_name = rom.str("UAE");

    lay.rts = rom.here();
    rom.w(0x4e75);
    for n in 16..=40 {
        lay.call[n as usize] = rom.here();
        rom.trap(n);
        rom.w(0x4e75);
    }
    for &(n, slot) in &BLITS {
        lay.blit[n as usize] = rom.here();
        rom.w(0x48e7); // movem.l d0,-(sp)
        rom.w(0x8000);
        rom.trap(n);
        rom.w(0x4a80); // tst.l d0
        rom.w(0x4cdf); // movem.l (sp)+,d0
        rom.w(0x0001);
        rom.w(0x6604); // bne.s done
        rom.w(0x2f28); // move.l default(a0),-(sp)
        rom.w((slot + 4) as u16);
        rom.w(0x4e75); // rts
    }
    for (i, &slot) in DEFAULTS.iter().enumerate() {
        lay.defaults[i] = rom.here();
        rom.w(0x2f28); // move.l default(a0),-(sp)
        rom.w((slot + 4) as u16);
        rom.w(0x4e75);
    }
    // ResolvePixelClock: one clock for everything.
    lay.resolve_pixel_clock = rom.here();
    rom.l(0x2340_002c); // move.l d0,gmi_PixelClock(a1)
    rom.w(0x7000); // moveq #0,d0
    rom.l(0x137c_0062); // move.b #98,gmi_Numerator(a1)
    rom.w(0x002a);
    rom.l(0x137c_000e); // move.b #14,gmi_Denominator(a1)
    rom.w(0x002b);
    rom.w(0x4e75);
    lay.get_pixel_clock = rom.here();
    rom.w(0x203c); // move.l #clock,d0
    rom.l(100_227_260);
    rom.w(0x4e75);
    // CalculateMemory: chunky only, so the size is what was asked for.
    lay.calculate_memory = rom.here();
    rom.w(0x2009); // move.l a1,d0
    rom.w(0x4e75);
    // GetCompatibleFormats: all formats coexist.
    lay.get_compatible_formats = rom.here();
    rom.w(0x203c);
    rom.l(0x3ffe);
    rom.w(0x4e75);

    // Library vectors: Open, Close, Expunge, the reserved one, FindCard, InitCard.
    let open = rom.here();
    rom.w(0x526e); // addq.w #1,lib_OpenCnt(a6)
    rom.w(0x0020);
    rom.w(0x08ae); // bclr #LIBB_DELEXP,lib_Flags(a6)
    rom.w(0x0003);
    rom.w(0x000e);
    rom.w(0x200e); // move.l a6,d0
    rom.w(0x4e75);
    let close = rom.here();
    rom.w(0x536e); // subq.w #1,lib_OpenCnt(a6)
    rom.w(0x0020);
    rom.w(0x7000); // moveq #0,d0
    rom.w(0x4e75);
    let null = rom.here();
    rom.w(0x7000);
    rom.w(0x4e75);
    let functions = rom.here();
    for f in [open, close, null, null, lay.call[16], lay.call[29], u32::MAX] {
        rom.l(f);
    }

    // The resident's init (a6 = ExecBase): MakeLibrary, fill in the node and
    // version, AddLibrary.
    let init = rom.here();
    rom.w(0x48e7); // movem.l a2,-(sp)
    rom.w(0x0020);
    rom.w(0x41f9); // lea functions,a0
    rom.l(functions);
    rom.w(0x93c9); // sub.l a1,a1
    rom.w(0x95ca); // sub.l a2,a2
    rom.w(0x203c); // move.l #size,d0
    rom.l(64);
    rom.w(0x7200); // moveq #0,d1
    rom.w(0x4eae); // jsr MakeLibrary(a6)
    rom.w(0xffac);
    rom.w(0x4a80); // tst.l d0
    rom.w(0x672e); // beq.s done
    rom.w(0x2240); // movea.l d0,a1
    rom.l(0x137c_0009); // move.b #NT_LIBRARY,ln_Type(a1)
    rom.w(0x0008);
    rom.w(0x237c); // move.l #name,ln_Name(a1)
    rom.l(name);
    rom.w(0x000a);
    rom.l(0x137c_0006); // move.b #LIBF_SUMUSED|LIBF_CHANGED,lib_Flags(a1)
    rom.w(0x000e);
    rom.w(0x337c); // move.w #version,lib_Version(a1)
    rom.w(LIBRARY_VERSION);
    rom.w(0x0014);
    rom.w(0x337c); // move.w #revision,lib_Revision(a1)
    rom.w(LIBRARY_REVISION);
    rom.w(0x0016);
    rom.w(0x237c); // move.l #id,lib_IdString(a1)
    rom.l(id);
    rom.w(0x0018);
    rom.w(0x4eae); // jsr AddLibrary(a6)
    rom.w(0xfe74);
    rom.w(0x4cdf); // done: movem.l (sp)+,a2
    rom.w(0x0400);
    rom.w(0x4e75);

    // The resident, found by exec's scan of $F00000-$F7FFFF.
    let tag = rom.here();
    rom.w(0x4afc); // RTC_MATCHWORD
    rom.l(tag);
    rom.l(tag + 26); // rt_EndSkip
    rom.buf.extend_from_slice(&[1, LIBRARY_VERSION as u8, 9, 0]); // RTF_COLDSTART, version, NT_LIBRARY, priority
    rom.l(name);
    rom.l(id);
    rom.l(init);
    assert!(rom.buf.len() < (TRAP_PC - ROM_BASE) as usize);
    rom.buf.resize(ROM_SIZE, 0);
    (rom.buf, lay)
}

/// The emulator's ROM at $F00000.
pub fn boot_rom() -> Vec<u8> {
    build().0
}

pub struct Rtg {
    /// Index of the VRAM card on the expansion chain.
    pub card: usize,
    /// Bytes at the top of VRAM that hold the mode list.
    reserved: u32,
    found: bool,
    /// The board's switch: true while the RTG display is shown.
    pub on: bool,
    clut: [[u8; 3]; 256],
    width: u32,
    height: u32,
    format: u32,
    /// Amiga address of the displayed bitmap, and its width in pixels.
    address: u32,
    virtual_width: u32,
    xoffset: i32,
    yoffset: i32,
    /// The display converted to RGBA, rebuilt every frame while `on`.
    fb: Vec<u32>,
    fb_size: (usize, usize),
    layout: Layout,
    /// The hardware sprite (mouse pointer): shown, top left, size, colour
    /// indices (0 is transparent) and colours 1-3 as RGBA.
    sprite_on: bool,
    sprite_pos: (i32, i32),
    sprite_size: (usize, usize),
    sprite: Vec<u8>,
    sprite_colors: [u32; 4],
}

impl Rtg {
    pub fn new(card: usize) -> Self {
        Rtg {
            card,
            reserved: 0,
            found: false,
            on: false,
            clut: [[0; 3]; 256],
            width: 0,
            height: 0,
            format: RGBFB_CLUT,
            address: 0,
            virtual_width: 0,
            xoffset: 0,
            yoffset: 0,
            fb: vec![],
            fb_size: (0, 0),
            layout: build().1,
            sprite_on: false,
            sprite_pos: (0, 0),
            sprite_size: (0, 0),
            sprite: vec![],
            sprite_colors: [0; 4],
        }
    }

    pub fn reset(&mut self) {
        *self = Rtg::new(self.card);
    }

    /// The converted display, when the board's output is shown.
    pub fn framebuffer(&self) -> Option<(&[u32], usize, usize)> {
        (self.on && !self.fb.is_empty()).then(|| (&self.fb[..], self.fb_size.0, self.fb_size.1))
    }

    /// Convert the displayed bitmap to RGBA (once per frame).
    pub fn render(&mut self, vram: &[u8], vram_base: u32) {
        let (w, h) = (self.width as usize, self.height as usize);
        let bpp = bytes_per_pixel(self.format) as usize;
        if !self.on || w == 0 || h == 0 || bpp == 0 {
            return;
        }
        self.fb.resize(w * h, 0xff00_0000);
        self.fb_size = (w, h);
        let bpr = self.virtual_width.max(self.width) as usize * bpp;
        let start = self.address.wrapping_sub(vram_base) as i64 + self.xoffset as i64 * bpp as i64 + self.yoffset as i64 * bpr as i64;
        let clut = &self.clut;
        let px = |p: &[u8]| -> u32 {
            let rgb = |r: u32, g: u32, b: u32| r | g << 8 | b << 16 | 0xff00_0000;
            let c5 = |v: u32| v << 3 | v >> 2;
            let c6 = |v: u32| v << 2 | v >> 4;
            match self.format {
                RGBFB_CLUT => {
                    let c = clut[p[0] as usize];
                    rgb(c[0] as u32, c[1] as u32, c[2] as u32)
                }
                RGBFB_R8G8B8 => rgb(p[0] as u32, p[1] as u32, p[2] as u32),
                RGBFB_B8G8R8 => rgb(p[2] as u32, p[1] as u32, p[0] as u32),
                RGBFB_A8R8G8B8 => rgb(p[1] as u32, p[2] as u32, p[3] as u32),
                RGBFB_A8B8G8R8 => rgb(p[3] as u32, p[2] as u32, p[1] as u32),
                RGBFB_R8G8B8A8 => rgb(p[0] as u32, p[1] as u32, p[2] as u32),
                RGBFB_B8G8R8A8 => rgb(p[2] as u32, p[1] as u32, p[0] as u32),
                _ => {
                    // The PC formats are little-endian words.
                    let v = match self.format {
                        RGBFB_R5G6B5PC | RGBFB_R5G5B5PC | RGBFB_B5G6R5PC | RGBFB_B5G5R5PC => u16::from_le_bytes([p[0], p[1]]),
                        _ => u16::from_be_bytes([p[0], p[1]]),
                    } as u32;
                    match self.format {
                        RGBFB_R5G6B5PC | RGBFB_R5G6B5 => rgb(c5(v >> 11), c6(v >> 5 & 63), c5(v & 31)),
                        RGBFB_B5G6R5PC => rgb(c5(v & 31), c6(v >> 5 & 63), c5(v >> 11)),
                        RGBFB_B5G5R5PC => rgb(c5(v & 31), c5(v >> 5 & 31), c5(v >> 10 & 31)),
                        _ => rgb(c5(v >> 10 & 31), c5(v >> 5 & 31), c5(v & 31)),
                    }
                }
            }
        };
        for y in 0..h {
            let row = start + (y * bpr) as i64;
            let out = &mut self.fb[y * w..(y + 1) * w];
            for (x, o) in out.iter_mut().enumerate() {
                let a = row + (x * bpp) as i64;
                *o = match usize::try_from(a).ok().and_then(|a| vram.get(a..a + bpp)) {
                    Some(p) => px(p),
                    None => 0xff00_0000,
                };
            }
        }
        if self.sprite_on {
            let (sw, sh) = self.sprite_size;
            let (sx, sy) = self.sprite_pos;
            for y in 0..sh {
                for x in 0..sw {
                    let c = self.sprite[y * sw + x];
                    let (px, py) = (sx + x as i32, sy + y as i32);
                    if c != 0 && px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h {
                        self.fb[py as usize * w + px as usize] = self.sprite_colors[c as usize];
                    }
                }
            }
        }
    }
}

fn read32(b: &mut Board, a: u32) -> u32 {
    (b.read16(a) as u32) << 16 | b.read16(a.wrapping_add(2)) as u32
}

fn write32(b: &mut Board, a: u32, v: u32) {
    b.write16(a, (v >> 16) as u16);
    b.write16(a.wrapping_add(2), v as u16);
}

/// Handle a call to the driver's trap address, then return as RTS would.
pub fn trap(cpu: &mut Cpu, board: &mut Board) {
    let sp = cpu.a[7];
    let num = read32(board, sp.wrapping_add(4));
    let result = call(num, cpu, board);
    cpu.d[0] = result;
    cpu.pc = read32(board, sp);
    cpu.a[7] = sp.wrapping_add(4);
    cpu.flush_prefetch();
}

fn call(num: u32, cpu: &mut Cpu, board: &mut Board) -> u32 {
    let (a0, a1) = (cpu.a[0], cpu.a[1]);
    let (d0, d1, d2, d7) = (cpu.d[0], cpu.d[1], cpu.d[2], cpu.d[7]);
    log::debug!("rtg: function {num} a0={a0:08x} a1={a1:08x} d0={d0:x} d1={d1:x} d7={d7:x}");
    match num {
        16 => find_card(board, a0),
        18 => {
            // SetSwitch: d0.w is the new state; returns the old one.
            let rtg = board.rtg.as_mut().unwrap();
            let old = rtg.on;
            rtg.on = d0 & 0xffff != 0;
            log::info!("rtg: {} display", if rtg.on { "graphics card" } else { "Amiga" });
            old as u32
        }
        19 => {
            // SetColorArray: d0.w first colour, d1.w count, from BoardInfo->CLUT.
            let (start, count) = ((d0 & 0xffff) as usize, (d1 & 0xffff) as usize);
            for i in start..(start + count).min(256) {
                let a = a0 + BI_CLUT + i as u32 * 3;
                let c = [board.read8(a), board.read8(a + 1), board.read8(a + 2)];
                board.rtg.as_mut().unwrap().clut[i] = c;
            }
            1
        }
        20 => 1, // SetDAC: the format comes with SetPanning.
        21 => {
            // SetGC: a1 ModeInfo, d0 border.
            write32(board, a0 + BI_MODE_INFO, a1);
            board.write16(a0 + BI_BORDER, d0 as u16);
            let w = board.read16(a1 + MI_WIDTH) as u32;
            let h = board.read16(a1 + MI_HEIGHT) as u32;
            let rtg = board.rtg.as_mut().unwrap();
            rtg.width = w;
            rtg.height = h;
            rtg.virtual_width = w;
            1
        }
        22 => {
            // SetPanning: a1 bitmap memory, d0.w width, d1.w/d2.w offsets, d7 format.
            board.write16(a0 + BI_XOFFSET, d1 as u16);
            board.write16(a0 + BI_YOFFSET, d2 as u16);
            let bme = read32(board, a0 + BI_BITMAP_EXTRA);
            let vw = if bme != 0 { board.read16(bme + BME_WIDTH) as u32 } else { d0 & 0xffff };
            let _vh = if bme != 0 { board.read16(bme + BME_HEIGHT) } else { 0 };
            let rtg = board.rtg.as_mut().unwrap();
            rtg.address = a1;
            rtg.xoffset = d1 as u16 as i16 as i32;
            rtg.yoffset = d2 as u16 as i16 as i32;
            rtg.virtual_width = vw;
            rtg.format = d7;
            1
        }
        23 => (d0 & 0xffff) * bytes_per_pixel(d7), // CalculateBytesPerRow
        26 => (d0 == 0) as u32,                    // SetDisplay: returns the opposite state.
        29 => init_card(board, a0),
        35 => 1, // Is there video memory?
        36 => {
            // SetSprite: d0 activate.
            if d0 != 0 {
                set_sprite_image(board, a0);
            }
            board.rtg.as_mut().unwrap().sprite_on = d0 != 0;
            1
        }
        37 => {
            // SetSpritePosition: from BoardInfo->MouseX/Y.
            let x = board.read16(a0 + BI_MOUSE_X) as i16 as i32;
            let y = board.read16(a0 + BI_MOUSE_Y) as i16 as i32;
            let rtg = board.rtg.as_mut().unwrap();
            rtg.sprite_pos = (x - rtg.xoffset, y - rtg.yoffset);
            1
        }
        38 => {
            set_sprite_image(board, a0);
            1
        }
        39 => {
            // SetSpriteColor: d0.b index 0-2 (colours 1-3), d1-d3 red, green, blue.
            let i = (d0 & 0xff) as usize + 1;
            if i < 4 {
                let c = (d1 & 0xff) | (d2 & 0xff) << 8 | (cpu.d[3] & 0xff) << 16 | 0xff00_0000;
                board.rtg.as_mut().unwrap().sprite_colors[i] = c;
            }
            1
        }
        17 => blit::fill_rect(cpu, board),
        24 => blit::planar_to_chunky(cpu, board),
        25 => blit::blit_rect(cpu, board),
        27 => blit::blit_template(cpu, board),
        28 => blit::blit_rect_no_mask_complete(cpu, board),
        30 => blit::blit_pattern(cpu, board),
        31 => blit::invert_rect(cpu, board),
        32 => blit::planar_to_direct(cpu, board),
        40 => blit::blit_rect_transparent(cpu, board),
        _ => 0,
    }
}

/// SetSpriteImage: two bitplanes from BoardInfo->MouseImage, after a header
/// of the same size as one line (words, or longs for a hires sprite; a "big"
/// lores sprite doubles every pixel).
fn set_sprite_image(board: &mut Board, bi: u32) {
    let w = board.read8(bi + BI_MOUSE_WIDTH) as usize;
    let h = board.read8(bi + BI_MOUSE_HEIGHT) as usize;
    let flags = read32(board, bi + BI_FLAGS);
    let image = read32(board, bi + BI_MOUSE_IMAGE);
    let hires = if flags & BIF_HIRESSPRITE != 0 { 2 } else { 1 };
    let double = flags & BIF_BIGSPRITE != 0 && hires == 1;
    let mut data = vec![0u8; w * h];
    if image != 0 {
        let line = 4 * hires as u32;
        let src_h = if double { h.div_ceil(2) } else { h };
        for sy in 0..src_h {
            let a = image + line + sy as u32 * line;
            let p0 = read32(board, a);
            let p1 = read32(board, a + 2 * hires as u32);
            for sx in 0..(16 * hires).min(w) {
                let c = (p0 >> (31 - sx) & 1 | (p1 >> (31 - sx) & 1) << 1) as u8;
                let scale = if double { 2 } else { 1 };
                for dy in 0..scale {
                    for dx in 0..scale {
                        let (x, y) = (sx * scale + dx, sy * scale + dy);
                        if x < w && y < h {
                            data[y * w + x] = c;
                        }
                    }
                }
            }
        }
    }
    let rtg = board.rtg.as_mut().unwrap();
    rtg.sprite = data;
    rtg.sprite_size = (w, h);
}

fn vram_base(board: &Board) -> Option<(u32, u32)> {
    let rtg = board.rtg.as_ref()?;
    let card = &board.exp.cards[rtg.card];
    Some((card.base?, card.ac.size))
}

fn mode_list_size() -> u32 {
    MODES.len() as u32 * (LIB_RESOLUTION_SIZE + DEPTHS.len() as u32 * MODE_INFO_SIZE) + 16
}

/// FindCard: a0 BoardInfo. Report the VRAM below the reserved mode list.
fn find_card(board: &mut Board, bi: u32) -> u32 {
    let Some((base, size)) = vram_base(board) else { return 0 };
    let reserved = mode_list_size();
    write32(board, bi + BI_MEMORY_BASE, base);
    write32(board, bi + BI_MEMORY_SIZE, size - reserved);
    let rtg = board.rtg.as_mut().unwrap();
    rtg.reserved = reserved;
    rtg.found = true;
    log::info!("rtg: uaegfx card with {} KB at {base:08x}", size / 1024);
    u32::MAX
}

/// InitCard: a0 BoardInfo. Describe the board and build its resolution list
/// in the reserved memory at the top of VRAM.
fn init_card(board: &mut Board, bi: u32) -> u32 {
    let Some((base, size)) = vram_base(board) else { return 0 };
    if !board.rtg.as_ref().unwrap().found {
        return 0;
    }
    let lay = board.rtg.as_ref().unwrap().layout;
    let mut mem = base + size - mode_list_size() + 16;
    let name = lay.board_name;
    board.write16(bi + BI_BITS_PER_CANNON, 8);
    board.write16(bi + BI_RGB_FORMATS, FORMATS as u16);
    write32(board, bi + BI_BOARD_TYPE, BT_UAEGFX);
    write32(board, bi + BI_GRAPHICS_CONTROLLER_TYPE, 0);
    write32(board, bi + BI_PALETTE_CHIP_TYPE, 0);
    write32(board, bi + BI_BOARD_NAME, name);
    write32(board, bi + BI_MEMORY_CLOCK, 200_000_000);
    for i in 0..5 {
        write32(board, bi + BI_PIXEL_CLOCK_COUNT + i * 4, 1);
        board.write16(bi + BI_MAX_HOR_VALUE + i * 2, 0x4000);
        board.write16(bi + BI_MAX_VER_VALUE + i * 2, 0x4000);
    }
    let mut flags = read32(board, bi + BI_FLAGS) & 0xffff_0000;
    flags |= BIF_BLITTER | BIF_NOMEMORYMODEMIX | BIF_GRANTDIRECTACCESS | BIF_INDISPLAYCHAIN | BIF_HARDWARESPRITE;
    write32(board, bi + BI_FLAGS, flags);
    // The BoardInfo functions, as WinUAE's in-memory uaegfx.card sets them.
    for &(n, slot) in &CALLS {
        write32(board, bi + slot, lay.call[n as usize]);
    }
    // The blitter stubs fall back to rtg.library's routine in the Default slot
    // after each if the emulator declines; Amiga Forever's rtg.library leaves
    // those empty and the card does all the drawing.
    for &(n, slot) in &BLITS {
        let stub = if read32(board, bi + slot + 4) != 0 { lay.blit[n as usize] } else { lay.call[n as usize] };
        write32(board, bi + slot, stub);
    }
    for (i, &slot) in DEFAULTS.iter().enumerate() {
        if read32(board, bi + slot + 4) != 0 {
            write32(board, bi + slot, lay.defaults[i]);
        }
    }
    for &slot in &NOTHING {
        write32(board, bi + slot, lay.rts);
    }
    write32(board, bi + BI_RESOLVE_PIXEL_CLOCK, lay.resolve_pixel_clock);
    write32(board, bi + BI_GET_PIXEL_CLOCK, lay.get_pixel_clock);
    write32(board, bi + BI_CALCULATE_MEMORY, lay.calculate_memory);
    write32(board, bi + BI_GET_COMPATIBLE_FORMATS, lay.get_compatible_formats);

    let (mut max_w, mut max_h) = (0u16, 0u16);
    for (n, &(w, h, index)) in MODES.iter().enumerate() {
        let res = mem;
        mem += LIB_RESOLUTION_SIZE;
        for a in (res..mem).step_by(2) {
            board.write16(a, 0);
        }
        for (i, &c) in b"P96-0:".iter().enumerate() {
            board.write8(res + LR_P96ID + i as u32, c);
        }
        let label = format!("UAE:{w:4}x{h:4}");
        for (i, c) in label.bytes().take(21).enumerate() {
            board.write8(res + LR_NAME + i as u32, c);
        }
        write32(board, res + 10, res + LR_P96ID); // ln_Name
        write32(board, res + LR_DISPLAY_ID, 0x5000_1000 | index << 16);
        board.write16(res + LR_WIDTH, w);
        board.write16(res + LR_HEIGHT, h);
        board.write16(res + LR_FLAGS, P96F_PUBLIC);
        write32(board, res + LR_BOARD_INFO, bi);
        for &depth in &DEPTHS {
            let bytes = w as u32 * h as u32 * bytes_per_pixel(format_for_depth(depth));
            if bytes > size - mode_list_size() {
                continue;
            }
            let mi = mem;
            mem += MODE_INFO_SIZE;
            for a in (mi..mem).step_by(2) {
                board.write16(a, 0);
            }
            board.write16(mi + MI_ACTIVE, 1);
            board.write16(mi + MI_WIDTH, w);
            board.write16(mi + MI_HEIGHT, h);
            board.write8(mi + MI_DEPTH, depth);
            board.write16(mi + MI_HOR_TOTAL, w + HOR_BLANK as u16);
            board.write16(mi + MI_HOR_BLANK_SIZE, HOR_BLANK as u16);
            board.write16(mi + MI_HOR_SYNC_START, 2);
            board.write16(mi + MI_HOR_SYNC_SIZE, 2);
            board.write16(mi + MI_VER_TOTAL, h + VER_BLANK as u16);
            board.write16(mi + MI_VER_BLANK_SIZE, VER_BLANK as u16);
            board.write16(mi + MI_VER_SYNC_START, 2);
            board.write16(mi + MI_VER_SYNC_SIZE, 2);
            board.write8(mi + MI_NUMERATOR, 98);
            board.write8(mi + MI_DENOMINATOR, 14);
            write32(board, mi + MI_PIXEL_CLOCK, (w as u32 + HOR_BLANK) * (h as u32 + VER_BLANK) * 60);
            // Modes[]: PLANAR, CHUNKY, HICOLOR, TRUECOLOR, TRUEALPHA.
            let slot = match depth {
                8 => 1,
                16 => 2,
                _ => 4,
            };
            write32(board, res + LR_MODES + slot * 4, mi);
        }
        max_w = max_w.max(w);
        max_h = max_h.max(h);
        // AddTail(&bi->ResolutionsList, res)
        let list = bi + BI_RESOLUTIONS_LIST;
        let tail_pred = read32(board, list + 8);
        write32(board, res, list + 4);
        write32(board, res + 4, tail_pred);
        write32(board, tail_pred, res);
        write32(board, list + 8, res);
        log::debug!("rtg: mode {n}: {w}x{h} id {:08x}", 0x5000_1000 | index << 16);
    }
    for i in 1..5 {
        board.write16(bi + BI_MAX_HOR_RESOLUTION + i * 2, max_w);
        board.write16(bi + BI_MAX_VER_RESOLUTION + i * 2, max_h);
    }
    u32::MAX
}
