mod frontend;
mod launcher;

use amiga::board::Amiga;
use amiga::config::{Config, Model};
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(version, about = "Commodore Amiga emulator")]
struct Args {
    /// Disk image or package to run: .adf (inserted in DF0) or .rp9.
    media: Option<PathBuf>,
    /// Kickstart ROM image (raw or Amiga Forever encrypted). If omitted, a ROM
    /// suitable for the model is looked up in --rom-dir.
    #[arg(long)]
    rom: Option<PathBuf>,
    /// Extension ROM mapped at $E00000 (e.g. aros-*-ext.rom, CD32 extended ROM).
    #[arg(long)]
    ext_rom: Option<PathBuf>,
    /// Directory holding Amiga Forever named ROMs (amiga-os-130.rom etc.) and rom.key.
    #[arg(long)]
    rom_dir: Option<PathBuf>,
    /// rom.key for encrypted ROMs (defaults to rom.key next to the ROM).
    #[arg(long)]
    rom_key: Option<PathBuf>,
    /// Machine model.
    #[arg(long, value_enum)]
    model: Option<Model>,
    /// ADF images for DF0..DF3.
    #[arg(long)]
    df0: Option<PathBuf>,
    #[arg(long)]
    df1: Option<PathBuf>,
    #[arg(long)]
    df2: Option<PathBuf>,
    #[arg(long)]
    df3: Option<PathBuf>,
    /// Chip RAM in KB (overrides model default).
    #[arg(long)]
    chip: Option<u32>,
    /// Slow ("ranger"/trapdoor) RAM at $C00000 in KB.
    #[arg(long)]
    slow: Option<u32>,
    /// Fast RAM in KB: Zorro II (autoconfig), or motherboard RAM on the A3000/A4000.
    #[arg(long)]
    fast: Option<u32>,
    /// CPU speed as a multiple of a 7 MHz 68000 (default: per model; 8 for
    /// RP9 packages that ask for maximum speed).
    #[arg(long)]
    cpu_speed: Option<f32>,
    /// CPU: 68000, 68020, 68030 or 68040 (default: per model).
    #[arg(long)]
    cpu: Option<String>,
    /// Floating-point unit: none, 68881 or 68882 (default: per model).
    #[arg(long)]
    fpu: Option<String>,
    /// Zorro III fast RAM in KB (A3000/A4000).
    #[arg(long)]
    z3: Option<u32>,
    #[arg(long)]
    ntsc: bool,
    /// Number of floppy drives (1-4).
    #[arg(long)]
    drives: Option<usize>,
    /// Stereo separation 0.0 (mono) to 1.0 (hard-panned, like real hardware).
    #[arg(long, default_value_t = 1.0)]
    stereo_separation: f32,
    /// Speed up floppy DMA.
    #[arg(long)]
    turbo_floppy: bool,
    /// Run without a window for N frames.
    #[arg(long)]
    headless: bool,
    #[arg(long, default_value_t = 500)]
    frames: u64,
    /// Save a PNG of the final frame (headless mode).
    #[arg(long)]
    screenshot: Option<PathBuf>,
    /// Extra ADFs for the disk swapper (PageDown cycles DF0 through df0 + these).
    #[arg(long = "swap")]
    swap: Vec<PathBuf>,
    /// Headless: draw the front-panel activity lights under screenshots.
    #[arg(long)]
    leds: bool,
    /// In headless mode, also save a screenshot every N frames (name_NNNNN.png).
    #[arg(long)]
    shot_every: Option<u64>,
    /// Headless: type keys at a frame, e.g. "1400:^D" or "1500:say hello\n".
    /// `^X` is Ctrl+X, `\n` is Return. May be given several times.
    #[arg(long = "type")]
    type_keys: Vec<String>,
    /// Headless: record audio output to a 16-bit stereo WAV file.
    #[arg(long)]
    wav: Option<PathBuf>,
    /// Print CPU state, recent PC history and a disassembly at exit (headless).
    #[arg(long)]
    debug_dump: bool,
    /// Write-protect inserted disks.
    #[arg(long)]
    write_protect: bool,
    /// Hard disk images (HDF) on an A590/A2091 SCSI controller, SCSI IDs 0.. in
    /// order. Writes stay in memory and are not saved back.
    #[arg(long = "hdf")]
    hdf: Vec<PathBuf>,
    /// Boot ROM for the SCSI controller (default: amiga-boot-a590.rom in --rom-dir).
    #[arg(long)]
    scsi_rom: Option<PathBuf>,
}

fn write_wav(path: &PathBuf, samples: &[f32], rate: u32) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    let data_len = samples.len() as u32 * 2;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&2u16.to_le_bytes())?;
    f.write_all(&rate.to_le_bytes())?;
    f.write_all(&(rate * 4).to_le_bytes())?;
    f.write_all(&4u16.to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    for s in samples {
        f.write_all(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())?;
    }
    Ok(())
}

/// Save the current picture, optionally with the activity-light panel below it.
fn save_shot(path: &PathBuf, amiga: &Amiga, panel: Option<(bool, u64)>) {
    let (fb, w, h) = amiga.board.framebuffer();
    let res = match panel {
        None => save_png(path, fb, w, h),
        Some((hd_active, tick)) => {
            let ph = amiga::panel::PANEL_H;
            let mut bytes: Vec<u8> = fb.iter().flat_map(|p| p.to_le_bytes()).collect();
            bytes.resize((h + ph) * w * 4, 0);
            amiga::panel::draw(&mut bytes, w, h, &amiga.board.leds(), hd_active, tick);
            let px: Vec<u32> = bytes.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
            save_png(path, &px, w, h + ph)
        }
    };
    res.expect("write screenshot");
}

fn save_png(path: &PathBuf, fb: &[u32], w: usize, h: usize) -> std::io::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc.write_header()?;
    let bytes: Vec<u8> = fb.iter().flat_map(|p| p.to_le_bytes()).collect();
    wr.write_image_data(&bytes)?;
    Ok(())
}

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("{msg}");
    std::process::exit(1)
}

fn rom_dirs(args: &Args) -> Vec<PathBuf> {
    let mut v = vec![];
    if let Some(d) = &args.rom_dir {
        v.push(d.clone());
    }
    for d in ["roms/af11", "roms"] {
        v.push(PathBuf::from(d));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.push(dir.join("roms"));
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        v.push(PathBuf::from(home).join(".config/amiga/roms"));
    }
    v
}

/// Locate an Amiga Forever shared file (`kind` is "hdf" or "adf"), such as
/// `workbench-135.hdf`, in ./<kind> or next to the ROM directories.
fn find_shared(args: &Args, kind: &str, name: &str) -> Option<PathBuf> {
    let mut dirs = vec![PathBuf::from(kind)];
    for d in rom_dirs(args) {
        dirs.push(d.join("..").join(kind));
        dirs.push(d.join(kind));
    }
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".config/amiga").join(kind));
    }
    dirs.into_iter().map(|d| d.join(name)).find(|p| p.is_file())
}

fn find_rom(args: &Args, candidates: &[&str]) -> PathBuf {
    for dir in rom_dirs(args) {
        for c in candidates {
            let p = dir.join(c);
            if p.is_file() {
                return p;
            }
        }
    }
    die(format!(
        "no Kickstart ROM found (looked for {} in {:?}); pass --rom or --rom-dir",
        candidates.join(", "),
        rom_dirs(args)
    ))
}

fn default_roms(model: Model) -> Vec<&'static str> {
    match model {
        Model::A500Plus => vec!["amiga-os-204.rom"],
        Model::A600 => vec!["amiga-os-205-a600.rom"],
        Model::A1200 => vec!["amiga-os-310-a1200.rom"],
        Model::A3000 => vec!["amiga-os-310-a3000.rom"],
        Model::A4000 => vec!["amiga-os-310-a4000.rom"],
        Model::Cd32 => vec!["amiga-os-310-cd32.rom"],
        _ => vec!["amiga-os-130.rom"],
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("error,amiga=info")).init();
    // With no arguments, open the graphical launcher.
    if std::env::args_os().len() == 1 {
        launcher::run();
        return;
    }
    let args = Args::parse();

    // Floppies to insert (name, data, writable path).
    let mut floppies: Vec<frontend::SwapDisk> = vec![];
    let mut rp9 = None;
    match &args.media {
        Some(p) if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("rp9")) => {
            let r = amiga::rp9::load(p).unwrap_or_else(|e| die(format!("{}: {e}", p.display())));
            log::info!("RP9: {} ({})", r.title, r.system);
            if r.floppy_names.is_empty() && r.harddrives.is_empty() && r.boot_hdf.is_none() && r.boot_adf.is_none() {
                die(format!("{}: {}", p.display(), r.unsupported_reason().unwrap_or_default()));
            }
            for (name, data) in &r.floppies {
                floppies.push(frontend::SwapDisk { name: name.clone(), data: data.clone(), path: None });
            }
            if let (true, Some(id)) = (r.floppies.is_empty(), &r.boot_adf) {
                let name = format!("amiga-os-{id}-workbench.adf");
                let p = find_shared(&args, "adf", &name).unwrap_or_else(|| {
                    die(format!("this title boots from {name}; put it in ./adf (from the Amiga Forever \"Shared/adf\" folder)"))
                });
                floppies.push(frontend::SwapDisk::from_path(&p).unwrap_or_else(|e| die(e)));
            }
            rp9 = Some(r);
        }
        Some(p) => floppies.push(frontend::SwapDisk::from_path(p).unwrap_or_else(|e| die(e))),
        None => {}
    }
    if let Some(p) = &args.df0 {
        floppies.insert(0, frontend::SwapDisk::from_path(p).unwrap_or_else(|e| die(e)));
    }
    for p in &args.swap {
        floppies.push(frontend::SwapDisk::from_path(p).unwrap_or_else(|e| die(e)));
    }

    let model = args.model.or(rp9.as_ref().and_then(|r| r.model())).unwrap_or(Model::A500);
    if !model.supported() {
        die(format!("model {model:?} is not supported yet (68020+/AGA is a later milestone)"));
    }
    let rom_path = match &args.rom {
        Some(p) => p.clone(),
        None => {
            // An explicit --model overrides the package's machine (and so its ROM).
            let cands = match &rp9 {
                Some(r) if args.model.is_none() || r.model() == Some(model) => r.rom_candidates(),
                _ => default_roms(model),
            };
            find_rom(&args, &cands)
        }
    };
    let rom = amiga::rom::load(&rom_path, args.rom_key.as_deref()).unwrap_or_else(|e| die(e));
    log::info!("Kickstart {}.{} ({} KB) from {}", rom.version, rom.revision, rom.data.len() / 1024, rom_path.display());

    let mut cfg = Config::for_model(model);
    if let Some(r) = &rp9 {
        if r.fast_ram > 0 {
            if model.addr32() {
                cfg.mb_kb = (r.fast_ram / 1024).min(16384);
            } else {
                cfg.fast_kb = (r.fast_ram / 1024).min(8192);
            }
        }
        if r.z3_ram > 0 {
            if model.addr32() {
                cfg.z3_kb = (r.z3_ram / 1024).min(1 << 20);
            } else {
                // No Zorro III on a 24-bit machine: give it Zorro II RAM instead.
                cfg.fast_kb = cfg.fast_kb.max((r.z3_ram / 1024).min(8192));
            }
        }
        if r.slow_ram > 0 {
            cfg.slow_kb = r.slow_ram / 1024;
        }
        if r.chip_ram > 0 {
            cfg.chip_kb = r.chip_ram / 1024;
        }
        cfg.turbo_floppy = r.turbo_floppy;
        cfg.drives = cfg.drives.max(r.extra_drives + 1);
    }
    if let Some(c) = args.chip {
        cfg.chip_kb = c;
    }
    if let Some(s) = args.slow {
        cfg.slow_kb = s;
    }
    if let Some(f) = args.fast {
        if model.addr32() {
            cfg.mb_kb = f.min(16384);
        } else {
            cfg.fast_kb = f;
        }
    }
    if let Some(z) = args.z3 {
        cfg.z3_kb = z;
    }
    if rp9.as_ref().is_some_and(|r| r.cpu_max) {
        cfg.cpu_speed = cfg.cpu_speed.max(8.0);
    }
    if let Some(sp) = args.cpu_speed {
        cfg.cpu_speed = sp.clamp(1.0, 64.0);
    }
    if let Some(c) = args.cpu.clone().or_else(|| rp9.as_ref().and_then(|r| r.cpu.clone())) {
        use amiga::cpu::{CpuModel, FpuModel};
        cfg.cpu = match c.as_str() {
            "68000" => CpuModel::M68000,
            "68020" | "68ec020" => CpuModel::M68020,
            "68030" => CpuModel::M68030,
            "68040" | "68060" => CpuModel::M68040,
            _ => die(format!("unknown CPU {c} (68000, 68020, 68030, 68040)")),
        };
        if cfg.cpu == CpuModel::M68040 {
            cfg.fpu = FpuModel::M68040;
        } else if cfg.fpu == FpuModel::M68040 {
            cfg.fpu = FpuModel::M68882;
        }
        if cfg.cpu != CpuModel::M68000 && !model.addr32() && model.cpu() == CpuModel::M68000 {
            log::warn!("{c} on a {model:?}: expect 24-bit addressing");
        }
    }
    let fpu_name = args.fpu.clone().or_else(|| rp9.as_ref().and_then(|r| r.fpu.clone()));
    if let Some(f) = fpu_name {
        cfg.fpu = match f.as_str() {
            "none" => amiga::cpu::FpuModel::None,
            "68881" => amiga::cpu::FpuModel::M68881,
            "68882" => amiga::cpu::FpuModel::M68882,
            _ => die(format!("unknown FPU {f} (none, 68881, 68882)")),
        };
    }
    cfg.ntsc = args.ntsc;
    cfg.turbo_floppy |= args.turbo_floppy;
    if let Some(d) = args.drives {
        cfg.drives = d.clamp(1, 4);
    }
    let extra = [&args.df1, &args.df2, &args.df3];
    if let Some(i) = extra.iter().rposition(|d| d.is_some()) {
        cfg.drives = cfg.drives.max(i + 2);
    }

    // Hard disks: an RP9's shared system disk boots first, then its own images.
    let mut hdfs: Vec<(String, Vec<u8>, bool)> = vec![];
    if let Some(r) = &rp9 {
        if let Some(b) = &r.boot_hdf {
            let p = find_shared(&args, "hdf", &format!("workbench-{b}.hdf")).unwrap_or_else(|| {
                die(format!("this title boots from workbench-{b}.hdf; put it in ./hdf (from the Amiga Forever \"Shared/hdf\" folder)"))
            });
            let data = std::fs::read(&p).unwrap_or_else(|e| die(format!("{}: {e}", p.display())));
            log::info!("boot hard disk {}", p.display());
            hdfs.push((format!("workbench-{b}.hdf"), data, true));
        }
        for (name, data) in &r.hdfs {
            hdfs.push((name.clone(), data.clone(), false));
        }
    }
    for p in &args.hdf {
        let data = std::fs::read(p).unwrap_or_else(|e| die(format!("{}: {e}", p.display())));
        hdfs.push((p.display().to_string(), data, false));
    }

    // Make every image mountable by Commodore's drivers: bare partitions get an
    // RDB, and all but the last disk let the driver scan on to later units.
    let n_hdfs = hdfs.len();
    for (i, (name, data, _)) in hdfs.iter_mut().enumerate() {
        let d = std::mem::take(data);
        *data = amiga::hdf::wrap_partition(d, &format!("DH{i}"), i == 0, if i == 0 { 0 } else { -1 });
        if i + 1 < n_hdfs {
            if let Some(f) = amiga::hdf::clear_last_flags(data) {
                log::info!("{name}: cleared RDB last-disk flags ({f:#x}) so later units are scanned");
            }
        }
    }

    let mut amiga = Amiga::new(cfg, rom.data);
    if !hdfs.is_empty() && amiga.board.gayle.is_some() {
        // A600/A1200: the internal IDE port takes a master and a slave.
        if hdfs.len() > 2 {
            die("the A600/A1200 IDE port takes at most 2 hard disks");
        }
        let g = amiga.board.gayle.as_mut().unwrap();
        for (i, (name, data, _ro)) in hdfs.into_iter().enumerate() {
            log::info!("IDE {}: {name} ({} KB)", if i == 0 { "master" } else { "slave" }, data.len() / 1024);
            g.drives[i] = Some(amiga::gayle::AtaDrive::new(name, data));
        }
    } else if !hdfs.is_empty() {
        if hdfs.len() > 7 {
            die("at most 7 hard disks are supported");
        }
        // The A3000's own SCSI controller is already fitted; elsewhere add an A590/A2091.
        let mut ctl = match amiga.board.scsi.take() {
            Some(c) => c,
            None => {
                let rom_path = args.scsi_rom.clone().unwrap_or_else(|| find_rom(&args, &["amiga-boot-a590.rom"]));
                let rom = amiga::rom::load_raw(&rom_path, args.rom_key.as_deref()).unwrap_or_else(|e| die(e));
                if rom.len() != 0x4000 {
                    die(format!("{}: expected a 16 KB A590/A2091 boot ROM", rom_path.display()));
                }
                amiga::a2091::A2091::new(rom)
            }
        };
        for (i, (name, data, ro)) in hdfs.into_iter().enumerate() {
            log::info!("SCSI unit {i}: {name} ({} KB)", data.len() / 1024);
            ctl.disks[i] = Some(amiga::a2091::HardDisk::new(name, data, ro));
        }
        amiga.board.scsi = Some(ctl);
    }
    amiga.board.audio.stereo_separation = args.stereo_separation.clamp(0.0, 1.0);
    let ext = args.ext_rom.clone().or_else(|| {
        // AROS ships its kernel split across a main and an extension ROM.
        let name = rom_path.file_name()?.to_str()?;
        let ext = rom_path.with_file_name(name.replace(".rom", "-ext.rom"));
        (name.starts_with("aros") && ext.is_file()).then_some(ext)
    });
    if let Some(p) = ext {
        let data = std::fs::read(&p).unwrap_or_else(|e| die(format!("{}: {e}", p.display())));
        let key = args.rom_key.clone().unwrap_or_else(|| p.with_file_name("rom.key"));
        amiga.board.ext_rom = if data.starts_with(b"AMIROMTYPE1") {
            let k = std::fs::read(&key).unwrap_or_else(|_| die("extension ROM is encrypted but rom.key is missing"));
            amiga::rom::decrypt(&data[11..], &k)
        } else {
            data
        };
        log::info!("extension ROM {} ({} KB)", p.display(), amiga.board.ext_rom.len() / 1024);
        amiga.hard_reset();
    }
    if let Some(d) = floppies.first() {
        amiga.board.drives[0].insert(d.to_disk(args.write_protect).unwrap_or_else(|e| die(e)));
    }
    // RP9 packages put their second disk in DF1 when a second drive is configured.
    if rp9.is_some() && amiga.board.drives[1].present && floppies.len() > 1 {
        amiga.board.drives[1].insert(floppies[1].to_disk(args.write_protect).unwrap_or_else(|e| die(e)));
    }
    for (i, d) in extra.iter().enumerate() {
        if let Some(p) = d {
            let disk = frontend::SwapDisk::from_path(p).and_then(|s| s.to_disk(args.write_protect));
            amiga.board.drives[i + 1].insert(disk.unwrap_or_else(|e| die(e)));
        }
    }

    if args.headless {
        let t = std::time::Instant::now();
        if args.debug_dump {
            amiga.pc_history = Some(Default::default());
            amiga.tracepoint = std::env::var("TRACE_AT").ok().and_then(|v| u32::from_str_radix(&v, 16).ok());
            amiga.trace_range = std::env::var("TRACE_RANGE").ok().and_then(|v| {
                let (lo, hi) = v.split_once('-')?;
                Some((u32::from_str_radix(lo, 16).ok()?, u32::from_str_radix(hi, 16).ok()?))
            });
            amiga.breakpoint = std::env::var("BREAK_AT").ok().and_then(|v| u32::from_str_radix(&v, 16).ok());
        }
        let mut audio_peak = 0f32;
        let mut recorded: Vec<f32> = vec![];
        let mut scripts: Vec<(u64, Vec<(u8, bool)>)> = args
            .type_keys
            .iter()
            .map(|t| {
                let (f, text) = t.split_once(':').unwrap_or_else(|| die("--type expects FRAME:TEXT"));
                (f.parse().unwrap_or_else(|_| die("bad frame in --type")), amiga::keyboard::text_to_keys(text))
            })
            .collect();
        let (mut hd_seen, mut hd_changed) = (0u64, 0u64);
        for f in 1..=args.frames {
            amiga.run_frame();
            if let Some(n) = amiga.board.leds().hd {
                if n != hd_seen {
                    hd_seen = n;
                    hd_changed = f;
                }
            }
            let panel = args.leds.then_some((hd_changed > 0 && f - hd_changed < 8, f));
            if amiga.break_hit {
                log::info!("breakpoint hit at frame {f}");
                break;
            }
            for s in amiga.board.take_audio() {
                audio_peak = audio_peak.max(s.abs());
                if args.wav.is_some() {
                    recorded.push(s);
                }
            }
            // Deliver scripted keys one event per frame, like a (fast) human typist.
            scripts.retain_mut(|(at, keys)| {
                if *at <= f && !keys.is_empty() {
                    let (k, down) = keys.remove(0);
                    amiga.board.keyboard.key(k, down);
                }
                !keys.is_empty()
            });
            if let (Some(n), Some(p)) = (args.shot_every, &args.screenshot) {
                if f % n == 0 {
                    let stem = p.with_extension("");
                    let path = PathBuf::from(format!("{}_{:05}.png", stem.display(), f));
                    save_shot(&path, &amiga, panel);
                }
            }
        }
        log::info!("audio peak level {:.3}", audio_peak);
        if let Some(p) = &args.wav {
            write_wav(p, &recorded, amiga.board.audio.sample_rate).expect("write wav");
        }
        let el = t.elapsed().as_secs_f64();
        log::info!(
            "{} frames in {:.2}s ({:.1} fps), pc={:06x}",
            args.frames,
            el,
            args.frames as f64 / el,
            amiga.cpu.pc
        );
        log::info!("{}", amiga.board.debug_state());
        if args.debug_dump {
            let c = &amiga.cpu;
            eprintln!("D: {:08x?}\nA: {:08x?}\nSR={:04x} USP={:08x} SSP={:08x}", c.d, c.a, c.sr(), c.usp(), c.ssp());
            if let Some(h) = &amiga.pc_history {
                let mut uniq: Vec<u32> = h.iter().copied().collect();
                uniq.sort();
                uniq.dedup();
                eprintln!("recent PCs (unique, {}): {:06x?}", uniq.len(), &uniq[..uniq.len().min(64)]);
                let tail: Vec<u32> = h.iter().rev().take(24).rev().copied().collect();
                eprintln!("last PCs: {:06x?}", tail);
            }
            if let Some((addr, len)) = std::env::var("DUMP_MEM").ok().and_then(|v| {
                let (a, l) = v.split_once(':')?;
                Some((u32::from_str_radix(a, 16).ok()?, l.parse::<u32>().ok()?))
            }) {
                for row in (0..len).step_by(16) {
                    let words: Vec<String> =
                        (0..8).map(|i| format!("{:04x}", amiga.board.peek16(addr + row + i * 2))).collect();
                    eprintln!("mem {:08x}: {}", addr + row, words.join(" "));
                }
            }
            let start = std::env::var("DISASM_AT").ok().and_then(|v| u32::from_str_radix(&v, 16).ok()).unwrap_or(amiga.cpu.pc);
            let n = std::env::var("DISASM_N").ok().and_then(|v| v.parse().ok()).unwrap_or(16);
            for l in amiga.disasm(start, n) {
                eprintln!("{l}");
            }
        }
        if !amiga.board.serial_out.is_empty() {
            log::info!("serial: {}", String::from_utf8_lossy(&amiga.board.serial_out));
        }
        if let Some(p) = &args.screenshot {
            let panel = args.leds.then_some((false, args.frames));
            save_shot(p, &amiga, panel);
        }
        for d in amiga.board.drives.iter_mut() {
            d.eject();
        }
        return;
    }
    frontend::run(amiga, frontend::Options { swap_disks: floppies, write_protect: args.write_protect });
}
