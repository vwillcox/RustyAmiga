//! MPEG-1 Layer II audio (ISO 11172-3): one frame in, 1152 stereo samples out.
//! Mono streams come out on both channels.

use super::Bits;
use std::sync::OnceLock;

/// Samples per channel in a Layer II frame.
pub const FRAME_SAMPLES: usize = 1152;

const BIT_RATES: [u32; 15] = [0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384];
const SAMPLE_RATES: [u32; 3] = [44100, 48000, 32000];

const MONO: u8 = 3;
const JOINT_STEREO: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// kbit/s.
    pub bit_rate: u32,
    pub sample_rate: u32,
    /// 0 stereo, 1 joint stereo, 2 dual channel, 3 mono.
    pub mode: u8,
    mode_extension: u8,
    crc: bool,
    /// Bytes in the frame, header included.
    pub size: usize,
}

impl Header {
    /// A Layer II frame header (MPEG-1 only) at the start of `h`.
    pub fn parse(h: &[u8]) -> Option<Header> {
        if h.len() < 4 || h[0] != 0xff || h[1] & 0xfe != 0xfc {
            return None;
        }
        let bit_rate = *BIT_RATES.get((h[2] >> 4) as usize)?;
        let sample_rate = *SAMPLE_RATES.get((h[2] >> 2 & 3) as usize)?;
        if bit_rate == 0 {
            return None; // free format
        }
        let padding = (h[2] >> 1 & 1) as usize;
        Some(Header {
            bit_rate,
            sample_rate,
            mode: h[3] >> 6,
            mode_extension: h[3] >> 4 & 3,
            crc: h[1] & 1 == 0,
            size: (144_000 * bit_rate / sample_rate) as usize + padding,
        })
    }
}

/// A quantisation class: steps, and bits per code (three samples share one
/// code when grouped).
#[derive(Clone, Copy)]
struct Quant {
    steps: u32,
    grouped: bool,
    bits: u32,
}

const fn q(steps: u32, grouped: bool, bits: u32) -> Quant {
    Quant { steps, grouped, bits }
}

/// The classes by number of steps: 3, 5, 7, 9, 15, 31 ... 65535.
const CLASSES: [Quant; 17] = [
    q(3, true, 5),
    q(5, true, 7),
    q(7, false, 3),
    q(9, true, 10),
    q(15, false, 4),
    q(31, false, 5),
    q(63, false, 6),
    q(127, false, 7),
    q(255, false, 8),
    q(511, false, 9),
    q(1023, false, 10),
    q(2047, false, 11),
    q(4095, false, 12),
    q(8191, false, 13),
    q(16383, false, 14),
    q(32767, false, 15),
    q(65535, false, 16),
];

/// Rows of the allocation tables (3-B.2a to d): the class (1-based index into
/// `CLASSES`) for each allocation code.
const ROWS: [&[u8]; 5] = [
    &[0, 1, 2, 17],
    &[0, 1, 2, 3, 4, 5, 6, 17],
    &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 17],
    &[0, 1, 3, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17],
    &[0, 1, 2, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
];

/// Allocation bits and row for each subband: high-rate tables 3-B.2a/b
/// (27 or 30 subbands) and low-rate tables 3-B.2c/d (8 or 12).
fn subband(high_rate: bool, sb: usize) -> (u32, usize) {
    if high_rate {
        match sb {
            0..=2 => (4, 3),
            3..=10 => (4, 2),
            11..=22 => (3, 1),
            _ => (2, 0),
        }
    } else {
        match sb {
            0..=1 => (4, 4),
            _ => (3, 4),
        }
    }
}

pub struct Decoder {
    /// The synthesis filter's 1024-sample history per channel, a ring
    /// starting at `v_start`.
    v: [[f32; 1024]; 2],
    v_start: usize,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    pub fn new() -> Self {
        Decoder { v: [[0.0; 1024]; 2], v_start: 0 }
    }

    /// Decode one frame (`frame` holds at least `Header::size` bytes) into
    /// 1152 stereo samples.
    pub fn decode(&mut self, frame: &[u8], out: &mut Vec<(i16, i16)>) -> Option<Header> {
        let h = Header::parse(frame)?;
        if frame.len() < h.size {
            return None;
        }
        let mut b = Bits::new(&frame[..h.size]);
        b.skip(32);
        if h.crc {
            b.skip(16);
        }
        let channels = if h.mode == MONO { 1 } else { 2 };
        let per_channel = h.bit_rate / channels as u32;
        let (high_rate, sblimit) = match (h.sample_rate, per_channel) {
            (48000, 56..) => (true, 27),
            (48000, _) => (false, 8),
            (_, 56..=80) => (true, 27),
            (_, 96..) => (true, 30),
            (32000, _) => (false, 12),
            _ => (false, 8),
        };
        let bound = match h.mode {
            JOINT_STEREO => ((h.mode_extension as usize + 1) * 4).min(sblimit),
            MONO => 0,
            _ => sblimit,
        };

        let mut alloc: [[Option<Quant>; 32]; 2] = [[None; 32]; 2];
        for sb in 0..sblimit {
            let (nbal, row) = subband(high_rate, sb);
            let read = |b: &mut Bits| match ROWS[row][b.read(nbal) as usize] {
                0 => None,
                c => Some(CLASSES[c as usize - 1]),
            };
            if sb < bound {
                for a in alloc.iter_mut() {
                    a[sb] = read(&mut b);
                }
            } else {
                let a = read(&mut b);
                alloc[0][sb] = a;
                alloc[1][sb] = a;
            }
        }
        let mut scfsi = [[0u32; 32]; 2];
        for sb in 0..sblimit {
            for ch in 0..channels {
                if alloc[ch][sb].is_some() {
                    scfsi[ch][sb] = b.read(2);
                }
            }
        }
        let mut scale = [[[0f32; 3]; 32]; 2];
        let factor = scale_factors();
        for sb in 0..sblimit {
            for ch in 0..channels {
                if alloc[ch][sb].is_none() {
                    continue;
                }
                let mut r = || factor[b.read(6) as usize];
                scale[ch][sb] = match scfsi[ch][sb] {
                    0 => [r(), r(), r()],
                    1 => {
                        let a = r();
                        [a, a, r()]
                    }
                    2 => [r(); 3],
                    _ => {
                        let a = r();
                        let c = r();
                        [a, c, c]
                    }
                };
            }
            if channels == 1 {
                scale[1][sb] = scale[0][sb];
            }
        }

        out.reserve(FRAME_SAMPLES);
        let mut samples = [[[0f32; 32]; 3]; 2];
        for granule in 0..12 {
            let part = granule / 4;
            for sb in 0..sblimit {
                // Requantised values in (-1, 1); above the bound one set of
                // codes serves both channels, each with its own scale factor.
                let mut frac = [0f32; 3];
                for ch in 0..2 {
                    if ch == 0 || sb < bound && channels == 2 {
                        frac = match alloc[ch][sb] {
                            Some(q) => read_codes(&mut b, q).map(|c| (2 * c as i32 - (q.steps as i32 - 1)) as f32 / q.steps as f32),
                            None => [0.0; 3],
                        };
                    }
                    for (s, f) in samples[ch].iter_mut().zip(frac) {
                        s[sb] = f * scale[ch][sb][part];
                    }
                }
            }
            for s in samples.iter_mut().flatten() {
                s[sblimit..].fill(0.0);
            }
            for (l, r) in samples[0].iter().zip(&samples[1]) {
                let mut pcm = [[0i16; 32]; 2];
                self.v_start = (self.v_start + 1024 - 64) & 1023;
                self.synthesise(0, l, &mut pcm[0]);
                self.synthesise(1, r, &mut pcm[1]);
                out.extend(pcm[0].iter().zip(&pcm[1]).map(|(&l, &r)| (l, r)));
            }
        }
        Some(h)
    }

    /// The polyphase synthesis filter: 32 subband samples to 32 PCM samples.
    fn synthesise(&mut self, ch: usize, s: &[f32; 32], pcm: &mut [i16; 32]) {
        let n = matrix();
        let v = &mut self.v[ch];
        for (i, row) in n.iter().enumerate() {
            v[(self.v_start + i) & 1023] = row.iter().zip(s).map(|(a, b)| a * b).sum();
        }
        for (j, out) in pcm.iter_mut().enumerate() {
            let mut sum = 0f32;
            for i in 0..8 {
                // U[64i + j] = V[128i + j] and U[64i + 32 + j] = V[128i + 96 + j].
                sum += v[(self.v_start + 128 * i + j) & 1023] * WINDOW[64 * i + j];
                sum += v[(self.v_start + 128 * i + 96 + j) & 1023] * WINDOW[64 * i + 32 + j];
            }
            *out = sum.round().clamp(-32768.0, 32767.0) as i16;
        }
    }
}

/// Three sample codes, read separately or split from one grouped code.
fn read_codes(b: &mut Bits, q: Quant) -> [u32; 3] {
    if q.grouped {
        let mut c = b.read(q.bits);
        std::array::from_fn(|_| {
            let v = c % q.steps;
            c /= q.steps;
            v
        })
    } else {
        std::array::from_fn(|_| b.read(q.bits))
    }
}

/// Scale factor index to multiplier: 2, 2^(2/3), 2^(1/3), 1 ...
fn scale_factors() -> &'static [f32; 64] {
    static T: OnceLock<[f32; 64]> = OnceLock::new();
    T.get_or_init(|| std::array::from_fn(|i| if i == 63 { 0.0 } else { 2f64.powf(1.0 - i as f64 / 3.0) as f32 }))
}

/// The matrixing coefficients N[i][k] = cos((16 + i)(2k + 1)pi / 64).
fn matrix() -> &'static [[f32; 32]; 64] {
    static T: OnceLock<[[f32; 32]; 64]> = OnceLock::new();
    T.get_or_init(|| std::array::from_fn(|i| std::array::from_fn(|k| (((16 + i) * (2 * k + 1)) as f64 * std::f64::consts::PI / 64.0).cos() as f32)))
}

/// The synthesis window D[i] of ISO 11172-3 Table 3-B.3, times 32768.
const WINDOW: [f32; 512] = [
    0.0, -0.5, -0.5, -0.5, -0.5, -0.5, -0.5, -1.0,
    -1.0, -1.0, -1.0, -1.5, -1.5, -2.0, -2.0, -2.5,
    -2.5, -3.0, -3.5, -3.5, -4.0, -4.5, -5.0, -5.5,
    -6.5, -7.0, -8.0, -8.5, -9.5, -10.5, -12.0, -13.0,
    -14.5, -15.5, -17.5, -19.0, -20.5, -22.5, -24.5, -26.5,
    -29.0, -31.5, -34.0, -36.5, -39.5, -42.5, -45.5, -48.5,
    -52.0, -55.5, -58.5, -62.5, -66.0, -69.5, -73.5, -77.0,
    -80.5, -84.5, -88.0, -91.5, -95.0, -98.0, -101.0, -104.0,
    106.5, 109.0, 111.0, 112.5, 113.5, 114.0, 114.0, 113.5,
    112.0, 110.5, 107.5, 104.0, 100.0, 94.5, 88.5, 81.5,
    73.0, 63.5, 53.0, 41.5, 28.5, 14.5, -1.0, -18.0,
    -36.0, -55.5, -76.5, -98.5, -122.0, -147.0, -173.5, -200.5,
    -229.5, -259.5, -290.5, -322.5, -355.5, -389.5, -424.0, -459.5,
    -495.5, -532.0, -568.5, -605.0, -641.5, -678.0, -714.0, -749.0,
    -783.5, -817.0, -849.0, -879.5, -908.5, -935.0, -959.5, -981.0,
    -1000.5, -1016.0, -1028.5, -1037.5, -1042.5, -1043.5, -1040.0, -1031.5,
    1018.5, 1000.0, 976.0, 946.5, 911.0, 869.5, 822.0, 767.5,
    707.0, 640.0, 565.5, 485.0, 397.0, 302.5, 201.0, 92.5,
    -22.5, -144.0, -272.5, -407.0, -547.5, -694.0, -846.0, -1003.0,
    -1165.0, -1331.5, -1502.0, -1675.5, -1852.5, -2031.5, -2212.5, -2394.0,
    -2576.5, -2758.5, -2939.5, -3118.5, -3294.5, -3467.5, -3635.5, -3798.5,
    -3955.0, -4104.5, -4245.5, -4377.5, -4499.0, -4609.5, -4708.0, -4792.5,
    -4863.5, -4919.0, -4958.0, -4979.5, -4983.0, -4967.5, -4931.5, -4875.0,
    -4796.0, -4694.5, -4569.5, -4420.0, -4246.0, -4046.0, -3820.0, -3567.0,
    3287.0, 2979.5, 2644.0, 2280.5, 1888.0, 1467.5, 1018.5, 541.0,
    35.0, -499.0, -1061.0, -1650.0, -2266.5, -2909.0, -3577.0, -4270.0,
    -4987.5, -5727.5, -6490.0, -7274.0, -8077.5, -8899.5, -9739.0, -10594.5,
    -11464.5, -12347.0, -13241.0, -14144.5, -15056.0, -15973.5, -16895.5, -17820.0,
    -18744.5, -19668.0, -20588.0, -21503.0, -22410.5, -23308.5, -24195.0, -25068.5,
    -25926.5, -26767.0, -27589.0, -28389.0, -29166.5, -29919.0, -30644.5, -31342.0,
    -32009.5, -32645.0, -33247.0, -33814.5, -34346.0, -34839.5, -35295.0, -35710.0,
    -36084.5, -36417.5, -36707.5, -36954.0, -37156.5, -37315.0, -37428.0, -37496.0,
    37519.0, 37496.0, 37428.0, 37315.0, 37156.5, 36954.0, 36707.5, 36417.5,
    36084.5, 35710.0, 35295.0, 34839.5, 34346.0, 33814.5, 33247.0, 32645.0,
    32009.5, 31342.0, 30644.5, 29919.0, 29166.5, 28389.0, 27589.0, 26767.0,
    25926.5, 25068.5, 24195.0, 23308.5, 22410.5, 21503.0, 20588.0, 19668.0,
    18744.5, 17820.0, 16895.5, 15973.5, 15056.0, 14144.5, 13241.0, 12347.0,
    11464.5, 10594.5, 9739.0, 8899.5, 8077.5, 7274.0, 6490.0, 5727.5,
    4987.5, 4270.0, 3577.0, 2909.0, 2266.5, 1650.0, 1061.0, 499.0,
    -35.0, -541.0, -1018.5, -1467.5, -1888.0, -2280.5, -2644.0, -2979.5,
    3287.0, 3567.0, 3820.0, 4046.0, 4246.0, 4420.0, 4569.5, 4694.5,
    4796.0, 4875.0, 4931.5, 4967.5, 4983.0, 4979.5, 4958.0, 4919.0,
    4863.5, 4792.5, 4708.0, 4609.5, 4499.0, 4377.5, 4245.5, 4104.5,
    3955.0, 3798.5, 3635.5, 3467.5, 3294.5, 3118.5, 2939.5, 2758.5,
    2576.5, 2394.0, 2212.5, 2031.5, 1852.5, 1675.5, 1502.0, 1331.5,
    1165.0, 1003.0, 846.0, 694.0, 547.5, 407.0, 272.5, 144.0,
    22.5, -92.5, -201.0, -302.5, -397.0, -485.0, -565.5, -640.0,
    -707.0, -767.5, -822.0, -869.5, -911.0, -946.5, -976.0, -1000.0,
    1018.5, 1031.5, 1040.0, 1043.5, 1042.5, 1037.5, 1028.5, 1016.0,
    1000.5, 981.0, 959.5, 935.0, 908.5, 879.5, 849.0, 817.0,
    783.5, 749.0, 714.0, 678.0, 641.5, 605.0, 568.5, 532.0,
    495.5, 459.5, 424.0, 389.5, 355.5, 322.5, 290.5, 259.5,
    229.5, 200.5, 173.5, 147.0, 122.0, 98.5, 76.5, 55.5,
    36.0, 18.0, 1.0, -14.5, -28.5, -41.5, -53.0, -63.5,
    -73.0, -81.5, -88.5, -94.5, -100.0, -104.0, -107.5, -110.5,
    -112.0, -113.5, -114.0, -114.0, -113.5, -112.5, -111.0, -109.0,
    106.5, 104.0, 101.0, 98.0, 95.0, 91.5, 88.0, 84.5,
    80.5, 77.0, 73.5, 69.5, 66.0, 62.5, 58.5, 55.5,
    52.0, 48.5, 45.5, 42.5, 39.5, 36.5, 34.0, 31.5,
    29.0, 26.5, 24.5, 22.5, 20.5, 19.0, 17.5, 15.5,
    14.5, 13.0, 12.0, 10.5, 9.5, 8.5, 8.0, 7.0,
    6.5, 5.5, 5.0, 4.5, 4.0, 3.5, 3.5, 3.0,
    2.5, 2.5, 2.0, 2.0, 1.5, 1.5, 1.0, 1.0,
    1.0, 1.0, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_sizes() {
        // 224 kbit/s, 44.1 kHz stereo: the Video CD rate.
        let h = Header::parse(&[0xff, 0xfd, 0xb0, 0x04]).unwrap();
        assert_eq!((h.bit_rate, h.sample_rate, h.size, h.crc), (224, 44100, 731, false));
        assert!(Header::parse(&[0xff, 0xfb, 0xb0, 0x04]).is_none(), "layer III");
    }

    #[test]
    fn silence_decodes_to_silence() {
        // A frame with nothing allocated.
        let mut frame = vec![0u8; 731];
        frame[..4].copy_from_slice(&[0xff, 0xfd, 0xb0, 0x04]);
        let mut out = vec![];
        Decoder::new().decode(&frame, &mut out).unwrap();
        assert_eq!(out.len(), FRAME_SAMPLES);
        assert!(out.iter().all(|&s| s == (0, 0)));
    }
}
