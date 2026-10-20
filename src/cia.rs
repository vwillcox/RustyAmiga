//! MOS 8520 Complex Interface Adapter.

pub const ICR_TA: u8 = 0x01;
pub const ICR_TB: u8 = 0x02;
pub const ICR_ALARM: u8 = 0x04;
pub const ICR_SP: u8 = 0x08;
pub const ICR_FLAG: u8 = 0x10;

#[derive(Clone, Debug)]
pub struct Cia {
    pub pra: u8,
    pub prb: u8,
    pub ddra: u8,
    pub ddrb: u8,
    /// Levels driven onto the port pins from outside (inputs).
    pub pa_in: u8,
    pub pb_in: u8,

    ta: u16,
    tb: u16,
    ta_latch: u16,
    tb_latch: u16,
    pub cra: u8,
    pub crb: u8,

    icr: u8,
    icr_mask: u8,

    tod: u32,
    tod_alarm: u32,
    tod_latched: Option<u32>,
    tod_halted: bool,

    sdr: u8,
    /// Remaining timer A underflows until an outgoing serial byte completes.
    sp_out_count: u8,
    /// Set when timer A underflowed during the last tick (for timer B cascading).
    ta_underflowed: bool,
}

impl Default for Cia {
    fn default() -> Self {
        Self::new()
    }
}

impl Cia {
    pub fn new() -> Self {
        Cia {
            pra: 0,
            prb: 0,
            ddra: 0,
            ddrb: 0,
            pa_in: 0xff,
            pb_in: 0xff,
            ta: 0xffff,
            tb: 0xffff,
            ta_latch: 0xffff,
            tb_latch: 0xffff,
            cra: 0,
            crb: 0,
            icr: 0,
            icr_mask: 0,
            tod: 0,
            tod_alarm: 0,
            tod_latched: None,
            tod_halted: false,
            sdr: 0,
            sp_out_count: 0,
            ta_underflowed: false,
        }
    }

    pub fn reset(&mut self) {
        let (pa, pb) = (self.pa_in, self.pb_in);
        *self = Cia::new();
        self.pa_in = pa;
        self.pb_in = pb;
    }

    /// Output level of port A pins (undriven pins float high).
    pub fn pa_out(&self) -> u8 {
        (self.pra & self.ddra) | !self.ddra
    }

    pub fn pb_out(&self) -> u8 {
        (self.prb & self.ddrb) | !self.ddrb
    }

    pub fn irq(&self) -> bool {
        self.icr & self.icr_mask & 0x1f != 0
    }

    pub fn set_icr(&mut self, bits: u8) {
        self.icr |= bits;
    }

    /// Serial input from the keyboard: latch a byte into SDR.
    pub fn serial_in(&mut self, byte: u8) {
        self.sdr = byte;
        self.icr |= ICR_SP;
    }

    pub fn sp_output_mode(&self) -> bool {
        self.cra & 0x40 != 0
    }

    /// One E-clock tick (CPU clock / 10).
    pub fn tick(&mut self) {
        self.ta_underflowed = false;
        if self.cra & 0x01 != 0 && self.cra & 0x20 == 0 {
            if self.ta == 0 {
                self.timer_a_underflow();
            } else {
                self.ta -= 1;
            }
        }
        if self.crb & 0x01 != 0 {
            let count = match (self.crb >> 5) & 3 {
                0 => true,
                2 | 3 => self.ta_underflowed,
                _ => false,
            };
            if count {
                if self.tb == 0 {
                    self.timer_b_underflow();
                } else {
                    self.tb -= 1;
                }
            }
        }
    }

    fn timer_a_underflow(&mut self) {
        self.icr |= ICR_TA;
        self.ta = self.ta_latch;
        self.ta_underflowed = true;
        if self.cra & 0x08 != 0 {
            self.cra &= !0x01;
        }
        if self.sp_output_mode() && self.sp_out_count > 0 {
            self.sp_out_count -= 1;
            if self.sp_out_count == 0 {
                self.icr |= ICR_SP;
            }
        }
    }

    fn timer_b_underflow(&mut self) {
        self.icr |= ICR_TB;
        self.tb = self.tb_latch;
        if self.crb & 0x08 != 0 {
            self.crb &= !0x01;
        }
    }

    /// Advance the time-of-day counter (vsync for CIA-A, hsync for CIA-B).
    pub fn tod_tick(&mut self) {
        if self.tod_halted {
            return;
        }
        self.tod = (self.tod + 1) & 0xff_ffff;
        if self.tod == self.tod_alarm {
            self.icr |= ICR_ALARM;
        }
    }

    pub fn read(&mut self, reg: u8) -> u8 {
        match reg & 15 {
            0 => (self.pra & self.ddra) | (self.pa_in & !self.ddra),
            1 => (self.prb & self.ddrb) | (self.pb_in & !self.ddrb),
            2 => self.ddra,
            3 => self.ddrb,
            4 => self.ta as u8,
            5 => (self.ta >> 8) as u8,
            6 => self.tb as u8,
            7 => (self.tb >> 8) as u8,
            8 => {
                let v = self.tod_latched.unwrap_or(self.tod);
                self.tod_latched = None;
                v as u8
            }
            9 => (self.tod_latched.unwrap_or(self.tod) >> 8) as u8,
            10 => {
                if self.tod_latched.is_none() {
                    self.tod_latched = Some(self.tod);
                }
                (self.tod_latched.unwrap() >> 16) as u8
            }
            11 => 0xff,
            12 => self.sdr,
            13 => {
                let v = self.icr | if self.irq() { 0x80 } else { 0 };
                self.icr = 0;
                v
            }
            14 => self.cra & !0x10,
            _ => self.crb & !0x10,
        }
    }

    pub fn write(&mut self, reg: u8, v: u8) {
        match reg & 15 {
            0 => self.pra = v,
            1 => self.prb = v,
            2 => self.ddra = v,
            3 => self.ddrb = v,
            4 => self.ta_latch = (self.ta_latch & 0xff00) | v as u16,
            5 => {
                self.ta_latch = (self.ta_latch & 0x00ff) | (v as u16) << 8;
                if self.cra & 0x01 == 0 {
                    self.ta = self.ta_latch;
                }
                // 8520: writing the high byte in one-shot mode starts the timer.
                if self.cra & 0x08 != 0 {
                    self.ta = self.ta_latch;
                    self.cra |= 0x01;
                }
            }
            6 => self.tb_latch = (self.tb_latch & 0xff00) | v as u16,
            7 => {
                self.tb_latch = (self.tb_latch & 0x00ff) | (v as u16) << 8;
                if self.crb & 0x01 == 0 {
                    self.tb = self.tb_latch;
                }
                if self.crb & 0x08 != 0 {
                    self.tb = self.tb_latch;
                    self.crb |= 0x01;
                }
            }
            8 | 9 | 10 => {
                let shift = (reg as u32 - 8) * 8;
                let target = if self.crb & 0x80 != 0 { &mut self.tod_alarm } else { &mut self.tod };
                *target = (*target & !(0xff << shift)) | (v as u32) << shift;
                if self.crb & 0x80 == 0 {
                    // Writing the high byte halts TOD until the low byte is written.
                    self.tod_halted = reg == 10 || (reg != 8 && self.tod_halted);
                }
            }
            11 => {}
            12 => {
                self.sdr = v;
                if self.sp_output_mode() {
                    self.sp_out_count = 16;
                }
            }
            13 => {
                if v & 0x80 != 0 {
                    self.icr_mask |= v & 0x1f;
                } else {
                    self.icr_mask &= !(v & 0x1f);
                }
            }
            14 => {
                if v & 0x10 != 0 {
                    self.ta = self.ta_latch;
                }
                self.cra = v & !0x10;
            }
            _ => {
                if v & 0x10 != 0 {
                    self.tb = self.tb_latch;
                }
                self.crb = v & !0x10;
            }
        }
    }
}
