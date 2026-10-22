//! Battery-backed real-time clocks at $DC0000: the OKI MSM6242B (A2000, A500+,
//! CDTV, and A501/A601-style trapdoor boards) and the Ricoh RF5C01A (A3000,
//! A4000). Each register is a nibble in the low four bits of the byte at
//! $DC0000 + 4n + 3. Time runs from the host clock plus an offset, so setting
//! the clock from AmigaOS works for the session.

use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RtcKind {
    Msm6242,
    Rf5c01,
}

pub struct Rtc {
    pub kind: RtcKind,
    /// Seconds added to the host time.
    offset: i64,
    /// MSM6242: control registers D, E, F.
    ctl: [u8; 3],
    /// RF5C01A: mode register (bank select in bits 0-1) and the 24-hour flag.
    mode: u8,
    h24: bool,
    /// RF5C01A battery-backed RAM: banks 2 and 3, 13 nibbles each.
    ram: [u8; 26],
    /// Time frozen while the HOLD bit is set (MSM6242) so reads are consistent.
    held: Option<i64>,
}

/// Broken-down local time.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Tm {
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    min: u32,
    sec: u32,
    weekday: u32,
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    // Howard Hinnant's algorithm: days since 1970-01-01.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d)
}

fn to_tm(t: i64) -> Tm {
    let days = t.div_euclid(86400);
    let s = t.rem_euclid(86400) as u32;
    let (year, month, day) = civil_from_days(days);
    Tm { year, month, day, hour: s / 3600, min: s / 60 % 60, sec: s % 60, weekday: (days + 4).rem_euclid(7) as u32 }
}

fn from_tm(tm: &Tm) -> i64 {
    days_from_civil(tm.year, tm.month, tm.day) * 86400 + (tm.hour * 3600 + tm.min * 60 + tm.sec) as i64
}

/// Seconds since 1970 in the host's local time zone, as an Amiga keeps it.
fn host_local_now() -> i64 {
    let utc = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    utc + local_utc_offset()
}

fn local_utc_offset() -> i64 {
    // The TZ offset from the C library (no extra crate needed).
    #[cfg(unix)]
    {
        #[repr(C)]
        struct CTm {
            tm_sec: i32,
            tm_min: i32,
            tm_hour: i32,
            tm_mday: i32,
            tm_mon: i32,
            tm_year: i32,
            tm_wday: i32,
            tm_yday: i32,
            tm_isdst: i32,
            tm_gmtoff: std::os::raw::c_long,
            tm_zone: *const std::os::raw::c_char,
        }
        extern "C" {
            fn localtime_r(t: *const i64, out: *mut CTm) -> *mut CTm;
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let mut tm = std::mem::MaybeUninit::<CTm>::zeroed();
        // SAFETY: localtime_r fills the struct we own; on failure it returns null.
        let ok = unsafe { !localtime_r(&now, tm.as_mut_ptr()).is_null() };
        if ok {
            // SAFETY: initialised by localtime_r above.
            return unsafe { tm.assume_init() }.tm_gmtoff as i64;
        }
    }
    0
}

/// Two-digit years as battclock.resource reads them: 78-99 are 1978-1999.
fn year_of(two_digits: u32) -> i64 {
    if two_digits >= 78 {
        1900 + two_digits as i64
    } else {
        2000 + two_digits as i64
    }
}

impl Rtc {
    pub fn new(kind: RtcKind) -> Self {
        // MSM6242 control F: 24-hour mode.
        Rtc { kind, offset: 0, ctl: [0, 0, 4], mode: 0, h24: true, ram: [0; 26], held: None }
    }

    fn now(&self) -> i64 {
        self.held.unwrap_or_else(|| host_local_now() + self.offset)
    }

    fn tm(&self) -> Tm {
        to_tm(self.now())
    }

    fn set_tm(&mut self, tm: Tm) {
        // Clamp to a valid date so a half-written date does not wrap months.
        let mut tm = tm;
        tm.month = tm.month.clamp(1, 12);
        tm.day = tm.day.clamp(1, 31);
        tm.hour = tm.hour.min(23);
        tm.min = tm.min.min(59);
        tm.sec = tm.sec.min(59);
        let t = from_tm(&tm);
        self.offset = t - host_local_now();
        if self.held.is_some() {
            self.held = Some(t);
        }
    }

    /// Read register `reg` (0-15); the high nibble reads as 0.
    pub fn read(&self, reg: usize) -> u8 {
        let tm = self.tm();
        let yy = tm.year.rem_euclid(100) as u32;
        let hour = |h24: bool| -> (u32, u32) {
            if h24 {
                (tm.hour % 10, tm.hour / 10)
            } else {
                let h12 = if tm.hour % 12 == 0 { 12 } else { tm.hour % 12 };
                let pm = if tm.hour >= 12 { 4 } else { 0 };
                (h12 % 10, h12 / 10 | pm)
            }
        };
        let v = match self.kind {
            RtcKind::Msm6242 => {
                let (h1, h10) = hour(self.ctl[2] & 4 != 0);
                match reg {
                    0 => tm.sec % 10,
                    1 => tm.sec / 10,
                    2 => tm.min % 10,
                    3 => tm.min / 10,
                    4 => h1,
                    5 => h10,
                    6 => tm.day % 10,
                    7 => tm.day / 10,
                    8 => tm.month % 10,
                    9 => tm.month / 10,
                    10 => yy % 10,
                    11 => yy / 10,
                    12 => tm.weekday,
                    13 => self.ctl[0] as u32 & !2, // never busy
                    14 => self.ctl[1] as u32,
                    _ => self.ctl[2] as u32,
                }
            }
            RtcKind::Rf5c01 => match (self.mode & 3, reg) {
                (_, 13) => self.mode as u32,
                (_, 14 | 15) => 0,
                (0, r) => {
                    let (h1, h10) = hour(self.h24);
                    match r {
                        0 => tm.sec % 10,
                        1 => tm.sec / 10,
                        2 => tm.min % 10,
                        3 => tm.min / 10,
                        4 => h1,
                        5 => h10,
                        6 => tm.weekday,
                        7 => tm.day % 10,
                        8 => tm.day / 10,
                        9 => tm.month % 10,
                        10 => tm.month / 10,
                        11 => yy % 10,
                        _ => yy / 10,
                    }
                }
                // Bank 1: 12/24-hour select and the leap-year counter.
                (1, 10) => self.h24 as u32,
                (1, 11) => (tm.year % 4) as u32,
                (1, _) => 0,
                (b, r) => self.ram[(b as usize - 2) * 13 + r] as u32,
            },
        };
        (v & 15) as u8
    }

    pub fn write(&mut self, reg: usize, v: u8) {
        let v = (v & 15) as u32;
        let mut tm = self.tm();
        let yy = tm.year.rem_euclid(100) as u32;
        let set_hour = |tm: &mut Tm, h1: Option<u32>, h10: Option<u32>, h24: bool| {
            if h24 {
                let (a, b) = (h1.unwrap_or(tm.hour % 10), h10.unwrap_or(tm.hour / 10));
                tm.hour = b * 10 + a;
            } else {
                let cur12 = if tm.hour % 12 == 0 { 12 } else { tm.hour % 12 };
                let pm = tm.hour >= 12;
                let a = h1.unwrap_or(cur12 % 10);
                let (b, pm) = match h10 {
                    Some(h) => (h & 3, h & 4 != 0),
                    None => (cur12 / 10, pm),
                };
                tm.hour = (b * 10 + a) % 12 + if pm { 12 } else { 0 };
            }
        };
        match self.kind {
            RtcKind::Msm6242 => {
                let h24 = self.ctl[2] & 4 != 0;
                match reg {
                    0 => tm.sec = tm.sec / 10 * 10 + v,
                    1 => tm.sec = v * 10 + tm.sec % 10,
                    2 => tm.min = tm.min / 10 * 10 + v,
                    3 => tm.min = v * 10 + tm.min % 10,
                    4 => set_hour(&mut tm, Some(v), None, h24),
                    5 => set_hour(&mut tm, None, Some(v), h24),
                    6 => tm.day = tm.day / 10 * 10 + v,
                    7 => tm.day = v * 10 + tm.day % 10,
                    8 => tm.month = tm.month / 10 * 10 + v,
                    9 => tm.month = v * 10 + tm.month % 10,
                    10 => tm.year = year_of(yy / 10 * 10 + v),
                    11 => tm.year = year_of(v * 10 + yy % 10),
                    12 => return, // the weekday follows from the date
                    13 => {
                        // HOLD freezes the counters for reading or setting.
                        let hold = v & 1 != 0;
                        if hold && self.held.is_none() {
                            self.held = Some(self.now());
                        } else if !hold && self.held.is_some() {
                            let t = self.held.take().unwrap();
                            self.offset = t - host_local_now();
                        }
                        self.ctl[0] = v as u8;
                        return;
                    }
                    14 => {
                        self.ctl[1] = v as u8;
                        return;
                    }
                    _ => {
                        self.ctl[2] = v as u8;
                        return;
                    }
                }
            }
            RtcKind::Rf5c01 => match (self.mode & 3, reg) {
                (_, 13) => {
                    self.mode = v as u8;
                    return;
                }
                (_, 14 | 15) => return,
                (0, r) => match r {
                    0 => tm.sec = tm.sec / 10 * 10 + v,
                    1 => tm.sec = v * 10 + tm.sec % 10,
                    2 => tm.min = tm.min / 10 * 10 + v,
                    3 => tm.min = v * 10 + tm.min % 10,
                    4 => set_hour(&mut tm, Some(v), None, self.h24),
                    5 => set_hour(&mut tm, None, Some(v), self.h24),
                    6 => return,
                    7 => tm.day = tm.day / 10 * 10 + v,
                    8 => tm.day = v * 10 + tm.day % 10,
                    9 => tm.month = tm.month / 10 * 10 + v,
                    10 => tm.month = v * 10 + tm.month % 10,
                    11 => tm.year = year_of(yy / 10 * 10 + v),
                    _ => tm.year = year_of(v * 10 + yy % 10),
                },
                (1, 10) => {
                    self.h24 = v & 1 != 0;
                    return;
                }
                (1, _) => return,
                (b, r) => {
                    self.ram[(b as usize - 2) * 13 + r] = v as u8;
                    return;
                }
            },
        }
        self.set_tm(tm);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_round_trip() {
        for t in [0i64, 951_782_400, 1_700_000_000, 4_102_444_800] {
            assert_eq!(from_tm(&to_tm(t)), t);
        }
        // 2000-02-29 was a Tuesday.
        let tm = to_tm(days_from_civil(2000, 2, 29) * 86400);
        assert_eq!((tm.year, tm.month, tm.day, tm.weekday), (2000, 2, 29, 2));
    }

    #[test]
    fn setting_the_clock_sticks() {
        for kind in [RtcKind::Msm6242, RtcKind::Rf5c01] {
            let mut r = Rtc::new(kind);
            // Write 1994-12-25 as battclock.resource would (year, month, day).
            let (y10, y1, mo10, mo1, d10, d1) = match kind {
                RtcKind::Msm6242 => (11, 10, 9, 8, 7, 6),
                RtcKind::Rf5c01 => (12, 11, 10, 9, 8, 7),
            };
            for (reg, v) in [(y10, 9), (y1, 4), (mo10, 1), (mo1, 2), (d10, 2), (d1, 5)] {
                r.write(reg, v);
            }
            let tm = r.tm();
            assert_eq!((tm.year, tm.month, tm.day), (1994, 12, 25), "{kind:?}");
            assert_eq!((r.read(y10), r.read(y1)), (9, 4));
        }
    }
}
