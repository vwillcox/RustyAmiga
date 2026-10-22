//! Graphical launcher: pick a machine (model, Kickstart, memory, drives) from
//! presets or by hand, or browse RP9 titles with their recommended setup.
//! The emulator itself runs as a child process of this same executable.

use amiga::config::Model;
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const COMPAT_DB: &str = include_str!("../data/compat.tsv");

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Machine {
    pub model: Model,
    pub rom: Option<PathBuf>,
    pub chip_kb: u32,
    pub slow_kb: u32,
    pub fast_kb: u32,
    pub drives: usize,
    pub ntsc: bool,
    pub turbo_floppy: bool,
    pub stereo_separation: f32,
    pub floppies: [Option<PathBuf>; 4],
    pub write_protect: bool,
    /// Add-on boards: (registry id, options such as "ram=4M").
    #[serde(default)]
    pub boards: Vec<(String, String)>,
    /// Hard disk images (HDF), in unit order.
    #[serde(default)]
    pub hard_disks: Vec<PathBuf>,
    /// CD image (ISO or CUE/BIN): the CD32 drive, a SCSI drive, or ATAPI on an IDE-only machine.
    #[serde(default)]
    pub cd: Option<PathBuf>,
}

impl Default for Machine {
    fn default() -> Self {
        Machine {
            model: Model::A500,
            rom: None,
            chip_kb: 512,
            slow_kb: 512,
            fast_kb: 0,
            drives: 1,
            ntsc: false,
            turbo_floppy: false,
            stereo_separation: 1.0,
            floppies: Default::default(),
            write_protect: false,
            boards: vec![],
            hard_disks: vec![],
            cd: None,
        }
    }
}

struct Preset {
    name: &'static str,
    note: &'static str,
    model: Model,
    roms: &'static [&'static str],
    chip_kb: u32,
    slow_kb: u32,
    fast_kb: u32,
}

const PRESETS: &[Preset] = &[
    Preset {
        name: "Amiga 500 · Kickstart 1.3 · 512K + 512K",
        note: "The classic games machine with the common trapdoor expansion. Best all-round choice.",
        model: Model::A500,
        roms: &["amiga-os-130.rom"],
        chip_kb: 512,
        slow_kb: 512,
        fast_kb: 0,
    },
    Preset {
        name: "Amiga 500 · Kickstart 1.3 · 512K",
        note: "Unexpanded A500, for titles that dislike extra memory.",
        model: Model::A500,
        roms: &["amiga-os-130.rom"],
        chip_kb: 512,
        slow_kb: 0,
        fast_kb: 0,
    },
    Preset {
        name: "Amiga 500 · Kickstart 1.2 · 512K + 512K",
        note: "Early A500; some 1987-88 software expects Kickstart 1.2.",
        model: Model::A500,
        roms: &["amiga-os-120.rom"],
        chip_kb: 512,
        slow_kb: 512,
        fast_kb: 0,
    },
    Preset {
        name: "Amiga 1000 · Kickstart 1.1 · 256K",
        note: "The original 1985 Amiga.",
        model: Model::A1000,
        roms: &["amiga-os-110-pal.rom", "amiga-os-110-ntsc.rom"],
        chip_kb: 256,
        slow_kb: 0,
        fast_kb: 0,
    },
    Preset {
        name: "Amiga 1000 · Kickstart 1.2 · 512K",
        note: "A1000 with the 256K front expansion.",
        model: Model::A1000,
        roms: &["amiga-os-120.rom"],
        chip_kb: 512,
        slow_kb: 0,
        fast_kb: 0,
    },
    Preset {
        name: "Amiga 2000 · Kickstart 1.3 · 512K + 512K + 2MB Fast",
        note: "Big-box A2000 with a Zorro II RAM card.",
        model: Model::A2000,
        roms: &["amiga-os-130.rom"],
        chip_kb: 512,
        slow_kb: 512,
        fast_kb: 2048,
    },
    Preset {
        name: "Amiga 500 Plus · Kickstart 2.04 · 1MB",
        note: "ECS A500+ (Workbench 2.x era).",
        model: Model::A500Plus,
        roms: &["amiga-os-204.rom"],
        chip_kb: 1024,
        slow_kb: 0,
        fast_kb: 0,
    },
    Preset {
        name: "Amiga 600 · Kickstart 2.05 · 1MB",
        note: "Stock A600.",
        model: Model::A600,
        roms: &["amiga-os-205-a600.rom"],
        chip_kb: 1024,
        slow_kb: 0,
        fast_kb: 0,
    },
    Preset {
        name: "Amiga 600 · Kickstart 3.1 · 1MB + 4MB Fast",
        note: "A600 upgraded to OS 3.1 with fast RAM.",
        model: Model::A600,
        roms: &["amiga-os-310-a600.rom", "amiga-os-3x0-a600.rom"],
        chip_kb: 1024,
        slow_kb: 0,
        fast_kb: 4096,
    },
];

const MODELS: &[(Model, &str)] = &[
    (Model::A1000, "Amiga 1000"),
    (Model::A500, "Amiga 500"),
    (Model::A2000, "Amiga 2000"),
    (Model::A500Plus, "Amiga 500 Plus"),
    (Model::A600, "Amiga 600"),
    (Model::A1200, "Amiga 1200"),
    (Model::A3000, "Amiga 3000"),
    (Model::A4000, "Amiga 4000"),
    (Model::A4000T, "Amiga 4000T"),
    (Model::Cd32, "CD32 (no CD drive yet)"),
];

fn model_name(m: Model) -> &'static str {
    MODELS.iter().find(|(x, _)| *x == m).map(|(_, n)| *n).unwrap_or("?")
}

/// Friendly names for Amiga Forever ROM files.
fn rom_friendly(file: &str) -> Option<&'static str> {
    Some(match file {
        "amiga-os-070.rom" => "Kickstart 0.7 (A1000 beta)",
        "amiga-os-100.rom" => "Kickstart 1.0 (A1000)",
        "amiga-os-110-ntsc.rom" => "Kickstart 1.1 NTSC (A1000)",
        "amiga-os-110-pal.rom" => "Kickstart 1.1 PAL (A1000)",
        "amiga-os-120.rom" => "Kickstart 1.2 (A500/A1000/A2000)",
        "amiga-os-130.rom" => "Kickstart 1.3 (A500/A2000)",
        "amiga-os-130-a3000.rom" => "Kickstart 1.3 (A3000 SuperKickstart)",
        "amiga-os-140-a3000.rom" => "Kickstart 1.4 beta (A3000)",
        "amiga-os-204.rom" => "Kickstart 2.04 (A500+)",
        "amiga-os-204-a3000.rom" => "Kickstart 2.04 (A3000)",
        "amiga-os-205-a600.rom" => "Kickstart 2.05 (A600)",
        "amiga-os-300-a1200.rom" => "Kickstart 3.0 (A1200)",
        "amiga-os-300-a4000.rom" => "Kickstart 3.0 (A4000)",
        "amiga-os-310-a600.rom" => "Kickstart 3.1 (A600)",
        "amiga-os-310-a1200.rom" => "Kickstart 3.1 (A1200)",
        "amiga-os-310-a3000.rom" => "Kickstart 3.1 (A3000)",
        "amiga-os-310-a4000.rom" => "Kickstart 3.1 (A4000)",
        "amiga-os-310-a4000t.rom" => "Kickstart 3.1 (A4000T)",
        "amiga-os-310-cd32.rom" => "Kickstart 3.1 (CD32)",
        "amiga-os-320-walker.rom" => "Kickstart 3.2 (Walker prototype)",
        "amiga-os-3x0-a600.rom" => "Kickstart 3.X (A600)",
        "amiga-os-3x0-a1200.rom" => "Kickstart 3.X (A1200)",
        "amiga-os-3x0-a3000.rom" => "Kickstart 3.X (A3000)",
        "amiga-os-3x0-a4000.rom" => "Kickstart 3.X (A4000)",
        "amiga-os-3x0-a4000t.rom" => "Kickstart 3.X (A4000T)",
        "aros-20250422.rom" => "AROS (open source, 2025-04-22)",
        "logica-dialoga-200.rom" => "Logica DiagROM / Dialoga 2.0",
        _ => return None,
    })
}

#[derive(Clone)]
struct RomEntry {
    path: PathBuf,
    label: String,
}

fn scan_roms(dirs: &[PathBuf]) -> Vec<RomEntry> {
    let mut out: Vec<RomEntry> = vec![];
    let skip = ["-ext.rom", "a570-ext", "cdtv-ext", "cd32-ext", "cd32-fmv", "boot-", "bonus-", "keyboard-", "picasso", "cyberstorm"];
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(d) else { continue };
        let mut files: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        files.sort();
        for p in files {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            if !name.ends_with(".rom") || skip.iter().any(|s| name.contains(s)) {
                continue;
            }
            if out.iter().any(|r| r.path.file_name() == p.file_name()) {
                continue;
            }
            let label = match rom_friendly(&name) {
                Some(f) => f.to_string(),
                None => match amiga::rom::load(&p, None) {
                    Ok(r) => format!("Kickstart {}.{} ({} KB) - {}", r.version, r.revision, r.data.len() / 1024, name),
                    Err(_) => continue,
                },
            };
            out.push(RomEntry { path: p, label });
        }
    }
    out
}

struct Title {
    path: PathBuf,
    info: amiga::rp9::Rp9,
    status: String,
    note: String,
}

fn scan_titles(dir: &Path, compat: &HashMap<String, (String, String)>) -> Vec<Title> {
    let mut paths = vec![];
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("rp9")) {
                paths.push(p);
            }
        }
    }
    let mut titles: Vec<Title> = paths
        .into_iter()
        .filter_map(|p| {
            let (info, _) = amiga::rp9::read_info(&p).ok()?;
            let stem = p.file_stem()?.to_string_lossy().to_string();
            let (mut status, mut note) = compat.get(&stem).cloned().unwrap_or(("untested".into(), String::new()));
            if let Some(r) = info.unsupported_reason() {
                status = "unsupported".into();
                note = r;
            }
            Some(Title { path: p, info, status, note })
        })
        .collect();
    titles.sort_by(|a, b| a.info.title.to_lowercase().cmp(&b.info.title.to_lowercase()));
    titles
}

#[derive(Serialize, Deserialize, Default)]
struct Saved {
    machine: Machine,
    titles_dir: Option<PathBuf>,
    rom_dir: Option<PathBuf>,
}

fn config_path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".config/amiga/launcher.json"))
}

#[derive(PartialEq)]
enum Tab {
    Machine,
    Games,
    Log,
}

struct App {
    tab: Tab,
    machine: Machine,
    roms: Vec<RomEntry>,
    rom_dirs: Vec<PathBuf>,
    rom_dir: Option<PathBuf>,
    titles_dir: PathBuf,
    titles: Vec<Title>,
    compat: HashMap<String, (String, String)>,
    filter: String,
    show_unsupported: bool,
    selected: Option<usize>,
    game_machine: Option<Machine>,
    preview: Option<(usize, Option<egui::TextureHandle>)>,
    log: Arc<Mutex<Vec<String>>>,
    running: Arc<Mutex<u32>>,
}

fn default_rom_dirs(extra: &Option<PathBuf>) -> Vec<PathBuf> {
    let mut v = vec![];
    if let Some(d) = extra {
        v.push(d.clone());
    }
    v.push("roms/af11".into());
    v.push("roms".into());
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

impl App {
    fn new() -> App {
        let saved: Saved = config_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let mut compat = HashMap::new();
        for line in COMPAT_DB.lines().filter(|l| !l.starts_with('#')) {
            let mut it = line.split('\t');
            if let (Some(k), Some(s)) = (it.next(), it.next()) {
                compat.insert(k.to_string(), (s.to_string(), it.next().unwrap_or("").to_string()));
            }
        }
        let rom_dirs = default_rom_dirs(&saved.rom_dir);
        let roms = scan_roms(&rom_dirs);
        let titles_dir = saved.titles_dir.clone().unwrap_or_else(|| PathBuf::from("titles"));
        let titles = scan_titles(&titles_dir, &compat);
        let mut app = App {
            tab: Tab::Machine,
            machine: saved.machine,
            roms,
            rom_dirs,
            rom_dir: saved.rom_dir,
            titles_dir,
            titles,
            compat,
            filter: String::new(),
            show_unsupported: false,
            selected: None,
            game_machine: None,
            preview: None,
            log: Arc::new(Mutex::new(vec![])),
            running: Arc::new(Mutex::new(0)),
        };
        if app.machine.rom.is_none() {
            app.apply_preset(&PRESETS[0]);
        }
        app
    }

    fn save_settings(&self) {
        let s = Saved { machine: self.machine.clone(), titles_dir: Some(self.titles_dir.clone()), rom_dir: self.rom_dir.clone() };
        if let Some(p) = config_path() {
            let _ = std::fs::create_dir_all(p.parent().unwrap());
            let _ = std::fs::write(p, serde_json::to_string_pretty(&s).unwrap());
        }
    }

    fn find_rom(&self, names: &[&str]) -> Option<PathBuf> {
        names.iter().find_map(|n| self.roms.iter().find(|r| r.path.file_name().is_some_and(|f| f == *n)).map(|r| r.path.clone()))
    }

    /// Newest AROS main ROM (`aros-YYYYMMDD.rom`; the extension ROM is found next to it).
    fn find_aros_rom(&self) -> Option<PathBuf> {
        self.roms
            .iter()
            .map(|r| &r.path)
            .filter(|p| {
                let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                n.starts_with("aros-") && n.ends_with(".rom") && !n.ends_with("-ext.rom")
            })
            .max_by_key(|p| p.file_name().map(|n| n.to_owned()))
            .cloned()
    }

    fn apply_preset(&mut self, p: &Preset) {
        self.machine.model = p.model;
        self.machine.rom = self.find_rom(p.roms);
        self.machine.chip_kb = p.chip_kb;
        self.machine.slow_kb = p.slow_kb;
        self.machine.fast_kb = p.fast_kb;
    }

    /// Recommended machine for a title, from its RP9 manifest.
    fn recommended(&self, t: &amiga::rp9::Rp9) -> Machine {
        let model = t.model().unwrap_or(Model::A500);
        let cfg = amiga::config::Config::for_model(model);
        Machine {
            model,
            rom: if t.rom.as_deref().is_some_and(|r| r.starts_with("aros")) {
                self.find_aros_rom()
            } else {
                self.find_rom(&t.rom_candidates())
            },
            chip_kb: if t.chip_ram > 0 { t.chip_ram / 1024 } else { cfg.chip_kb },
            slow_kb: if t.slow_ram > 0 { t.slow_ram / 1024 } else { cfg.slow_kb },
            // --fast sets motherboard RAM on the A3000/A4000: keep the model's.
            fast_kb: if t.fast_ram > 0 {
                (t.fast_ram / 1024).min(8192)
            } else {
                cfg.mb_kb
            },
            drives: (t.extra_drives + 1).max(if t.floppy_names.len() > 1 { 2 } else { 1 }).min(4),
            ntsc: false,
            turbo_floppy: t.turbo_floppy,
            stereo_separation: self.machine.stereo_separation,
            floppies: Default::default(),
            write_protect: false,
            boards: vec![],
            hard_disks: vec![],
            cd: None,
        }
    }

    fn launch(&mut self, m: &Machine, media: Option<&Path>) {
        let Ok(exe) = std::env::current_exe() else { return };
        let mut cmd = std::process::Command::new(exe);
        if let Some(p) = media {
            cmd.arg(p);
        }
        cmd.args(["--model", &format!("{:?}", m.model).to_lowercase().replace("a500plus", "a500-plus").replace("cd32", "cd32")]);
        if let Some(r) = &m.rom {
            cmd.arg("--rom").arg(r);
        }
        cmd.args(["--chip", &m.chip_kb.to_string(), "--slow", &m.slow_kb.to_string(), "--fast", &m.fast_kb.to_string()]);
        cmd.args(["--drives", &m.drives.to_string(), "--stereo-separation", &m.stereo_separation.to_string()]);
        for (id, opts) in &m.boards {
            cmd.arg("--board").arg(if opts.is_empty() { id.clone() } else { format!("{id}:{opts}") });
        }
        if m.ntsc {
            cmd.arg("--ntsc");
        }
        if m.turbo_floppy {
            cmd.arg("--turbo-floppy");
        }
        if m.write_protect {
            cmd.arg("--write-protect");
        }
        for (i, f) in m.floppies.iter().enumerate() {
            if let Some(f) = f {
                cmd.arg(format!("--df{i}")).arg(f);
            }
        }
        for h in &m.hard_disks {
            cmd.arg("--hdf").arg(h);
        }
        if let Some(c) = &m.cd {
            cmd.arg("--cd").arg(c);
        }
        cmd.stderr(std::process::Stdio::piped()).stdout(std::process::Stdio::piped());
        let desc = format!("{cmd:?}");
        self.log.lock().unwrap().push(format!("$ {desc}"));
        match cmd.spawn() {
            Ok(mut child) => {
                *self.running.lock().unwrap() += 1;
                let log = self.log.clone();
                let running = self.running.clone();
                let stderr = child.stderr.take();
                std::thread::spawn(move || {
                    if let Some(e) = stderr {
                        for line in std::io::BufReader::new(e).lines().map_while(Result::ok) {
                            log.lock().unwrap().push(line);
                        }
                    }
                    let status = child.wait();
                    log.lock().unwrap().push(format!("emulator exited: {status:?}"));
                    *running.lock().unwrap() -= 1;
                });
            }
            Err(e) => self.log.lock().unwrap().push(format!("failed to start emulator: {e}")),
        }
        self.save_settings();
    }

    fn machine_editor(ui: &mut egui::Ui, m: &mut Machine, roms: &[RomEntry], id: &str) {
        egui::Grid::new(id).num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("Model");
            egui::ComboBox::from_id_salt(format!("{id}-model")).width(320.0).selected_text(model_name(m.model)).show_ui(ui, |ui| {
                for (model, name) in MODELS {
                    ui.add_enabled_ui(model.supported(), |ui| {
                        if ui.selectable_value(&mut m.model, *model, *name).changed() {
                            let c = amiga::config::Config::for_model(*model);
                            m.chip_kb = c.chip_kb;
                            m.slow_kb = c.slow_kb;
                            if model.addr32() {
                                m.fast_kb = c.mb_kb;
                            }
                        }
                    });
                }
            });
            ui.end_row();

            ui.label("Kickstart ROM");
            let cur = m
                .rom
                .as_ref()
                .and_then(|p| roms.iter().find(|r| &r.path == p).map(|r| r.label.clone()).or(Some(p.display().to_string())))
                .unwrap_or_else(|| "(none found)".into());
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt(format!("{id}-rom")).width(320.0).selected_text(cur).show_ui(ui, |ui| {
                    for r in roms {
                        ui.selectable_value(&mut m.rom, Some(r.path.clone()), &r.label);
                    }
                });
                if ui.button("Browse…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("Kickstart ROM", &["rom", "bin"]).pick_file() {
                        m.rom = Some(p);
                    }
                }
            });
            ui.end_row();

            ui.label("Chip RAM");
            egui::ComboBox::from_id_salt(format!("{id}-chip")).selected_text(kb(m.chip_kb)).show_ui(ui, |ui| {
                let max = if matches!(m.model, Model::A500 | Model::A1000 | Model::A2000) { 1024 } else { 2048 };
                for v in [256, 512, 1024, 2048].into_iter().filter(|v| *v <= max) {
                    ui.selectable_value(&mut m.chip_kb, v, kb(v));
                }
            });
            ui.end_row();

            ui.label("Slow RAM ($C00000)");
            egui::ComboBox::from_id_salt(format!("{id}-slow")).selected_text(kb(m.slow_kb)).show_ui(ui, |ui| {
                for v in [0, 512, 1024, 1536] {
                    ui.selectable_value(&mut m.slow_kb, v, kb(v));
                }
            })
            .response
            .on_hover_text("Trapdoor / ranger memory (e.g. the A501 512K expansion)");
            ui.end_row();

            ui.label("Fast RAM (Zorro II)");
            egui::ComboBox::from_id_salt(format!("{id}-fast")).selected_text(kb(m.fast_kb)).show_ui(ui, |ui| {
                for v in [0, 512, 1024, 2048, 4096, 8192] {
                    ui.selectable_value(&mut m.fast_kb, v, kb(v));
                }
            })
            .response
            .on_hover_text("Autoconfig RAM expansion board");
            ui.end_row();

            ui.label("Floppy drives");
            ui.add(egui::Slider::new(&mut m.drives, 1..=4));
            ui.end_row();

            ui.label("Video");
            ui.horizontal(|ui| {
                ui.radio_value(&mut m.ntsc, false, "PAL (50 Hz)");
                ui.radio_value(&mut m.ntsc, true, "NTSC (60 Hz)");
            });
            ui.end_row();

            ui.label("Options");
            ui.horizontal(|ui| {
                ui.checkbox(&mut m.turbo_floppy, "Turbo floppy");
                ui.checkbox(&mut m.write_protect, "Write-protect disks");
            });
            ui.end_row();

            ui.label("Stereo separation");
            ui.add(egui::Slider::new(&mut m.stereo_separation, 0.0..=1.0).custom_formatter(|v, _| format!("{:.0}%", v * 100.0)))
                .on_hover_text("100% = hard-panned like a real Amiga");
            ui.end_row();
        });
    }

    /// Add-on boards: the ones fitted, and the ones that fit this model.
    fn expansion_editor(ui: &mut egui::Ui, m: &mut Machine) {
        use amiga::expansion::registry::{self, Options, BOARDS};
        let mut remove = None;
        egui::Grid::new("boards").num_columns(4).spacing([10.0, 4.0]).show(ui, |ui| {
            for (i, (id, opts)) in m.boards.iter_mut().enumerate() {
                let Some(info) = registry::find(id) else { continue };
                ui.label(info.name).on_hover_text(format!("{}, {}; {}", info.maker, info.year, info.slot.describe()));
                ui.add(egui::TextEdit::singleline(opts).desired_width(140.0).hint_text("options")).on_hover_text(info.options);
                let problem = if !info.slot.fits(m.model) {
                    Some(format!("does not fit the {}", model_name(m.model)))
                } else {
                    Options::parse(opts).and_then(|o| info.build(&o)).err()
                };
                match problem {
                    Some(e) => ui.colored_label(egui::Color32::LIGHT_RED, e),
                    None => ui.label(""),
                };
                if ui.small_button("Remove").clicked() {
                    remove = Some(i);
                }
                ui.end_row();
            }
        });
        if let Some(i) = remove {
            m.boards.remove(i);
        }
        egui::ComboBox::from_id_salt("add-board").width(320.0).selected_text("Add board…").show_ui(ui, |ui| {
            for b in BOARDS.iter().filter(|b| b.slot.fits(m.model)) {
                if ui.selectable_label(false, format!("{} ({})", b.name, b.maker)).on_hover_text(b.options).clicked() {
                    m.boards.push((b.id.to_string(), String::new()));
                }
            }
        });
    }

    fn floppy_editor(ui: &mut egui::Ui, m: &mut Machine) {
        for i in 0..m.drives {
            ui.horizontal(|ui| {
                ui.label(format!("DF{i}:"));
                let text = m.floppies[i].as_ref().and_then(|p| p.file_name()).map(|f| f.to_string_lossy().to_string()).unwrap_or("(empty)".into());
                ui.add_sized([360.0, 20.0], egui::Label::new(text).truncate());
                if ui.button("Insert…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("Amiga disk", &["adf"]).pick_file() {
                        m.floppies[i] = Some(p);
                    }
                }
                if m.floppies[i].is_some() && ui.button("Eject").clicked() {
                    m.floppies[i] = None;
                }
            });
        }
    }

    fn storage_editor(ui: &mut egui::Ui, m: &mut Machine) {
        let mut remove = None;
        for (i, h) in m.hard_disks.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(format!("Unit {i}:"));
                let text = h.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
                ui.add_sized([360.0, 20.0], egui::Label::new(text).truncate());
                if ui.button("Remove").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            m.hard_disks.remove(i);
        }
        if m.hard_disks.len() < 7 && ui.button("Add hard disk…").clicked() {
            if let Some(p) = rfd::FileDialog::new().add_filter("Hard disk image", &["hdf"]).pick_file() {
                m.hard_disks.push(p);
            }
        }
        ui.horizontal(|ui| {
            ui.label("CD-ROM:");
            let text = m.cd.as_ref().and_then(|p| p.file_name()).map(|f| f.to_string_lossy().to_string()).unwrap_or("(empty)".into());
            ui.add_sized([360.0, 20.0], egui::Label::new(text).truncate());
            if ui.button("Insert…").clicked() {
                if let Some(p) = rfd::FileDialog::new().add_filter("CD image", &["iso", "cue"]).pick_file() {
                    m.cd = Some(p);
                }
            }
            if m.cd.is_some() && ui.button("Eject").clicked() {
                m.cd = None;
            }
        });
        ui.label(egui::RichText::new("Hard disks go on the SCSI board chosen above, else the machine's own controller; the CD-ROM drive joins them (on the A600/A1200/A4000 IDE port it is ATAPI, which needs a Kickstart 3.X scsi.device). A CD32 uses its own drive.").weak());
    }

    fn machine_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Presets");
        ui.label("Known-good combinations. Presets only use ROMs found on this computer.");
        ui.add_space(4.0);
        let mut chosen = None;
        egui::Grid::new("presets").num_columns(2).spacing([10.0, 4.0]).show(ui, |ui| {
            for (i, p) in PRESETS.iter().enumerate() {
                let have = self.find_rom(p.roms).is_some();
                ui.add_enabled_ui(have, |ui| {
                    if ui.button(p.name).clicked() {
                        chosen = Some(i);
                    }
                });
                ui.label(egui::RichText::new(if have { p.note } else { "ROM not found" }).weak());
                ui.end_row();
            }
        });
        if let Some(i) = chosen {
            self.apply_preset(&PRESETS[i]);
        }
        ui.separator();
        ui.heading("Configuration");
        Self::machine_editor(ui, &mut self.machine, &self.roms, "main");
        ui.separator();
        ui.heading("Expansion boards");
        Self::expansion_editor(ui, &mut self.machine);
        ui.separator();
        ui.heading("Floppy disks");
        Self::floppy_editor(ui, &mut self.machine);
        ui.separator();
        ui.heading("Hard disks and CD-ROM");
        Self::storage_editor(ui, &mut self.machine);
        ui.add_space(10.0);
        let ok = self.machine.rom.is_some();
        if ui.add_enabled(ok, egui::Button::new(egui::RichText::new("▶  Start Amiga").size(18.0))).clicked() {
            let m = self.machine.clone();
            self.launch(&m, None);
        }
    }

    fn games_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.label("Search:");
            ui.text_edit_singleline(&mut self.filter);
            ui.checkbox(&mut self.show_unsupported, "Show titles that need unemulated hardware");
            if ui.button("Titles folder…").clicked() {
                if let Some(d) = rfd::FileDialog::new().pick_folder() {
                    self.titles_dir = d;
                    self.titles = scan_titles(&self.titles_dir, &self.compat);
                    self.selected = None;
                    self.save_settings();
                }
            }
        });
        ui.label(egui::RichText::new(format!("{} titles in {}", self.titles.len(), self.titles_dir.display())).weak());
        ui.separator();
        let f = self.filter.to_lowercase();
        let visible: Vec<usize> = (0..self.titles.len())
            .filter(|&i| {
                let t = &self.titles[i];
                (self.show_unsupported || t.status != "unsupported")
                    && (f.is_empty() || t.info.title.to_lowercase().contains(&f) || t.info.publisher.to_lowercase().contains(&f))
            })
            .collect();
        egui::SidePanel::left("title-list").resizable(true).default_width(380.0).show_inside(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                for &i in &visible {
                    let t = &self.titles[i];
                    let badge = match t.status.as_str() {
                        "ok" => egui::RichText::new("●").color(egui::Color32::from_rgb(80, 200, 120)),
                        "blank" => egui::RichText::new("●").color(egui::Color32::from_rgb(230, 170, 50)),
                        "unsupported" => egui::RichText::new("●").color(egui::Color32::from_rgb(200, 80, 80)),
                        _ => egui::RichText::new("●").color(egui::Color32::GRAY),
                    };
                    let label = format!("{}  ({}, {})", t.info.title, t.info.publisher, t.info.year);
                    let mut clicked = false;
                    ui.horizontal(|ui| {
                        ui.label(badge);
                        clicked = ui.selectable_label(self.selected == Some(i), label).clicked();
                    });
                    if clicked {
                        self.selected = Some(i);
                        self.game_machine = Some(self.recommended(&self.titles[i].info));
                    }
                }
            });
        });
        let Some(i) = self.selected else {
            ui.label("Select a title. Green = verified booting, amber = boots with issues, red = needs hardware not emulated yet.");
            return;
        };
        if self.preview.as_ref().map(|p| p.0) != Some(i) {
            let tex = amiga::rp9::read_info(&self.titles[i].path).ok().and_then(|(_, png)| png).and_then(|png| decode_png(&png)).map(|img| ctx.load_texture(format!("preview{i}"), img, Default::default()));
            self.preview = Some((i, tex));
        }
        let t = &self.titles[i];
        ui.heading(&t.info.title);
        ui.label(format!("{} · {} · {} · {}", t.info.publisher, t.info.year, t.info.kind, t.info.system));
        if let Some((_, Some(tex))) = &self.preview {
            ui.add(egui::Image::new(tex).max_width(320.0));
        }
        let status_text = match t.status.as_str() {
            "ok" => "✔ Verified: reaches its title screen in automated testing",
            "blank" => "⚠ Boots but showed no picture in automated testing",
            "unsupported" => "✖ Not runnable yet",
            _ => "Not yet tested",
        };
        ui.label(egui::RichText::new(status_text).strong());
        if !t.note.is_empty() && t.status == "unsupported" {
            ui.label(&t.note);
        }
        ui.label(format!("Disks: {}", if t.info.floppy_names.is_empty() { "hard disk image".to_string() } else { t.info.floppy_names.join(", ") }));
        ui.separator();
        ui.label(egui::RichText::new("Recommended configuration (from the package; you can change it)").strong());
        let path = t.path.clone();
        let runnable = t.status != "unsupported";
        let recommended = self.recommended(&self.titles[i].info);
        if let Some(gm) = self.game_machine.as_mut() {
            Self::machine_editor(ui, gm, &self.roms, "game");
        }
        ui.horizontal(|ui| {
            if ui.button("Reset to recommended").clicked() {
                self.game_machine = Some(recommended);
            }
            if ui.add_enabled(runnable, egui::Button::new(egui::RichText::new("▶  Play").size(18.0))).clicked() {
                if let Some(gm) = self.game_machine.clone() {
                    self.launch(&gm, Some(&path));
                }
            }
        });
    }
}

fn kb(v: u32) -> String {
    match v {
        0 => "None".into(),
        v if v >= 1024 && v % 1024 == 0 => format!("{} MB", v / 1024),
        v => format!("{v} KB"),
    }
}

fn decode_png(bytes: &[u8]) -> Option<egui::ColorImage> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => buf[..w * h * 4].to_vec(),
        png::ColorType::Rgb => buf[..w * h * 3].chunks(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect(),
        png::ColorType::Grayscale => buf[..w * h].iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf[..w * h * 2].chunks(2).flat_map(|c| [c[0], c[0], c[0], c[1]]).collect(),
        _ => return None,
    };
    Some(egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba))
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Amiga");
                ui.separator();
                ui.selectable_value(&mut self.tab, Tab::Machine, "Machine");
                ui.selectable_value(&mut self.tab, Tab::Games, format!("Games & Demos ({})", self.titles.len()));
                ui.selectable_value(&mut self.tab, Tab::Log, "Log");
                let n = *self.running.lock().unwrap();
                if n > 0 {
                    ui.label(egui::RichText::new(format!("● {n} running")).color(egui::Color32::from_rgb(80, 200, 120)));
                }
            });
        });
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(format!("{} Kickstart ROMs found", self.roms.len())).weak());
                if ui.small_button("ROM folder…").clicked() {
                    if let Some(d) = rfd::FileDialog::new().pick_folder() {
                        self.rom_dir = Some(d);
                        self.rom_dirs = default_rom_dirs(&self.rom_dir);
                        self.roms = scan_roms(&self.rom_dirs);
                        self.save_settings();
                    }
                }
                ui.label(egui::RichText::new("In the emulator: click to capture mouse, F12 release, F11 fullscreen, End warp, Home joystick keys, PageDown next disk").weak());
            });
        });
        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            Tab::Machine => {
                egui::ScrollArea::vertical().show(ui, |ui| self.machine_tab(ui));
            }
            Tab::Games => self.games_tab(ui, ctx),
            Tab::Log => {
                egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                    for l in self.log.lock().unwrap().iter() {
                        ui.monospace(l);
                    }
                });
            }
        });
        if *self.running.lock().unwrap() > 0 {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_settings();
    }
}

pub fn run() {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 760.0])
            .with_title("Amiga Launcher")
            .with_app_id(crate::frontend::APP_ID),
        ..Default::default()
    };
    if let Err(e) = eframe::run_native("Amiga Launcher", options, Box::new(|_cc| Ok(Box::new(App::new())))) {
        eprintln!("launcher failed: {e}");
    }
}
