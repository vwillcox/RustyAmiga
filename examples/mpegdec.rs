//! Decode an MPEG-1 video elementary stream to raw 4:2:0 YUV, or a Layer II
//! audio stream to 16-bit stereo PCM, with the FMV cartridge's decoders.
//!
//! mpegdec video IN.m1v OUT.yuv
//! mpegdec audio IN.mp2 OUT.pcm

use amiga::mpeg::{audio, video};
use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!("usage: mpegdec video|audio IN OUT");
        std::process::exit(1);
    }
    let data = std::fs::read(&args[2]).expect("input");
    let mut out = std::io::BufWriter::new(std::fs::File::create(&args[3]).expect("output"));
    let start = std::time::Instant::now();
    if args[1] == "video" {
        let mut d = video::Decoder::new();
        let mut frames = 0;
        // Feed it in sector-sized pieces, as the cartridge gets it.
        for chunk in data.chunks(2324) {
            d.push(chunk);
            while let Some(e) = d.next_event() {
                if let video::Event::Picture(f) = e {
                    out.write_all(&f.y).unwrap();
                    out.write_all(&f.cb).unwrap();
                    out.write_all(&f.cr).unwrap();
                    frames += 1;
                }
            }
        }
        d.end();
        while let Some(e) = d.next_event() {
            if let video::Event::Picture(f) = e {
                out.write_all(&f.y).unwrap();
                out.write_all(&f.cb).unwrap();
                out.write_all(&f.cr).unwrap();
                frames += 1;
            }
        }
        eprintln!("{frames} pictures in {:?}", start.elapsed());
    } else {
        let mut d = audio::Decoder::new();
        let mut pcm = vec![];
        let mut at = 0;
        let mut frames = 0;
        while let Some(h) = audio::Header::parse(&data[at.min(data.len())..]) {
            if d.decode(&data[at..], &mut pcm).is_none() {
                break;
            }
            at += h.size;
            frames += 1;
        }
        for (l, r) in pcm {
            out.write_all(&l.to_le_bytes()).unwrap();
            out.write_all(&r.to_le_bytes()).unwrap();
        }
        eprintln!("{frames} frames in {:?}", start.elapsed());
    }
}
