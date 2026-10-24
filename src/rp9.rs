//! Cloanto RetroPlatform RP9 packages: a zip with `rp9-manifest.xml` plus media.

use crate::config::Model;
use std::io::Read;
use std::path::Path;

#[derive(Debug, Default, Clone)]
pub struct Rp9 {
    pub title: String,
    pub year: String,
    pub publisher: String,
    /// "game", "demo", ...
    pub kind: String,
    /// CPU named by the package (e.g. "68020"), if any.
    pub cpu: Option<String>,
    /// FPU named by the package ("68881"/"68882"), if any.
    pub fpu: Option<String>,
    /// The package asks for the fastest CPU emulation (speed="max").
    pub cpu_max: bool,
    /// The package was authored for a JIT CPU core, i.e. a CPU as fast as the host allows.
    pub jit: bool,
    pub system: String,
    pub system_rom: Option<u32>,
    pub fast_ram: u32,
    pub z3_ram: u32,
    pub slow_ram: u32,
    pub chip_ram: u32,
    pub turbo_floppy: bool,
    pub extra_drives: usize,
    /// Video memory of UAE's virtual graphics card (`<peripheral type="synthetic">rtg`).
    pub rtg_mem: Option<u32>,
    /// Floppy image names in priority order.
    pub floppy_names: Vec<String>,
    /// Floppy images in priority order (only filled by `load`): (name, contents).
    pub floppies: Vec<(String, Vec<u8>)>,
    pub harddrives: Vec<String>,
    /// Hard disk images in priority order (only filled by `load`): (name, contents).
    pub hdfs: Vec<(String, Vec<u8>)>,
    /// Shared Amiga Forever system hard disk to boot from, e.g. "135" for
    /// `workbench-135.hdf`.
    pub boot_hdf: Option<String>,
    /// Shared Amiga Forever Workbench floppy to boot from, e.g. "310" for
    /// `amiga-os-310-workbench.adf`.
    pub boot_adf: Option<String>,
    /// Amiga Forever host folders mounted as hard disks, in priority order:
    /// (folder name under `Shared/dir`, volume name). The first is the boot
    /// disk when `<boot type="dir">` names the shared System folder.
    pub host_dirs: Vec<(String, String)>,
    /// Shared OS folder the package boots from (`<boot type="dir">`, e.g. "3x0").
    pub boot_dir: Option<String>,
    /// ROM named by the package (`<rom>`, e.g. "aros-latest").
    pub rom: Option<String>,
}

impl Rp9 {
    pub fn model(&self) -> Option<Model> {
        Some(match self.system.as_str() {
            "a-500" => Model::A500,
            "a-500plus" => Model::A500Plus,
            "a-600" => Model::A600,
            "a-1000" => Model::A1000,
            "a-2000" => Model::A2000,
            "a-1200" => Model::A1200,
            "a-3000" => Model::A3000,
            // AROS runs on any 68020+ Amiga; Amiga Forever gives it a big one.
            // The Walker prototype is an AGA 68030 machine with its own Kickstart.
            "a-4000" | "a-4xxx" | "a-aros" | "a-walker" => Model::A4000,
            "cd32" | "a-cd32" => Model::Cd32,
            "cdtv" | "a-cdtv" => Model::Cdtv,
            _ => return None,
        })
    }

    /// Candidate Amiga Forever ROM file names for this package, best first.
    pub fn rom_candidates(&self) -> Vec<&'static str> {
        let model = self.model();
        match (self.system_rom, model) {
            (Some(110), _) => vec!["amiga-os-110-pal.rom", "amiga-os-120.rom", "amiga-os-130.rom"],
            (Some(120), _) => vec!["amiga-os-120.rom", "amiga-os-130.rom"],
            (Some(130), _) => vec!["amiga-os-130.rom", "amiga-os-120.rom"],
            (_, Some(Model::A500Plus)) => vec!["amiga-os-204.rom"],
            (_, Some(Model::A600)) => vec!["amiga-os-205-a600.rom", "amiga-os-310-a600.rom"],
            (Some(310), Some(Model::A1200)) => vec!["amiga-os-310-a1200.rom"],
            (Some(310), _) => vec!["amiga-os-310-a4000.rom", "amiga-os-310-a1200.rom"],
            (_, Some(Model::A1200)) => vec!["amiga-os-310-a1200.rom"],
            (_, Some(Model::A3000)) => vec!["amiga-os-310-a3000.rom"],
            (_, Some(Model::A4000)) if self.system == "a-walker" => vec!["amiga-os-320-walker.rom"],
            (_, Some(Model::A4000)) if self.boot_dir.as_deref() == Some("3x0") => {
                vec!["amiga-os-3x0-a4000.rom", "amiga-os-310-a4000.rom"]
            }
            (_, Some(Model::A4000)) => vec!["amiga-os-310-a4000.rom"],
            (_, Some(Model::Cd32)) => vec!["amiga-os-310-cd32.rom"],
            _ => vec!["amiga-os-130.rom"],
        }
    }
}

fn tag_text<'a>(xml: &'a str, tag: &str) -> Vec<(&'a str, &'a str)> {
    // Returns (attributes, text) for every <tag ...>text</tag>.
    let mut out = vec![];
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut rest = xml;
    while let Some(i) = rest.find(&open) {
        let after = &rest[i + open.len()..];
        // Make sure this is the whole tag name.
        if !after.starts_with(['>', ' ', '\t', '/']) {
            rest = after;
            continue;
        }
        let Some(gt) = after.find('>') else { break };
        let attrs = &after[..gt];
        let body = &after[gt + 1..];
        if attrs.ends_with('/') {
            out.push((attrs, ""));
            rest = body;
            continue;
        }
        let Some(end) = body.find(&close) else { break };
        out.push((attrs, body[..end].trim()));
        rest = &body[end + close.len()..];
    }
    out
}

fn attr<'a>(attrs: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("{name}=\"");
    let i = attrs.find(&key)? + key.len();
    let end = attrs[i..].find('"')?;
    Some(&attrs[i..i + end])
}

/// Parse the manifest only (fast; no media is read). Also returns the preview PNG.
pub fn read_info(path: &Path) -> Result<(Rp9, Option<Vec<u8>>), String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("not a valid RP9 (zip): {e}"))?;
    let xml = manifest_text(&mut zip)?;
    let rp = parse_manifest(&xml);
    let mut preview = None;
    if let Ok(mut f) = zip.by_name("rp9-preview.png") {
        let mut v = vec![];
        if f.read_to_end(&mut v).is_ok() {
            preview = Some(v);
        }
    }
    Ok((rp, preview))
}

fn manifest_text<R: std::io::Read + std::io::Seek>(zip: &mut zip::ZipArchive<R>) -> Result<String, String> {
    let mut xml = String::new();
    zip.by_name("rp9-manifest.xml")
        .map_err(|_| "RP9 has no rp9-manifest.xml".to_string())?
        .read_to_string(&mut xml)
        .map_err(|e| e.to_string())?;
    Ok(xml.trim_start_matches('\u{feff}').to_string())
}

fn parse_manifest(xml: &str) -> Rp9 {
    let first = |tag: &str| tag_text(xml, tag).first().map(|(_, t)| t.to_string());
    let mut rp = Rp9 {
        title: first("title").unwrap_or_default(),
        year: first("year").unwrap_or_default(),
        kind: first("type").unwrap_or_default(),
        system: first("system").unwrap_or_default(),
        system_rom: first("systemrom").and_then(|s| s.parse().ok()),
        ..Default::default()
    };
    for (attrs, text) in tag_text(xml, "entity") {
        if attr(attrs, "type") == Some("publisher") && rp.publisher.is_empty() {
            rp.publisher = text.to_string();
        }
    }
    for (attrs, text) in tag_text(xml, "ram") {
        let bytes: u32 = text.parse().unwrap_or(0);
        match attr(attrs, "type") {
            Some("fast") => rp.fast_ram = bytes,
            Some("z3") => rp.z3_ram = bytes,
            Some("slow") => rp.slow_ram = bytes,
            Some("chip") => rp.chip_ram = bytes,
            _ => {}
        }
    }
    for (_, text) in tag_text(xml, "compatibility") {
        if text == "turbo-floppy" {
            rp.turbo_floppy = true;
        }
        rp.jit |= text == "jit";
    }
    for (attrs, text) in tag_text(xml, "peripheral") {
        rp.jit |= text == "jit";
        if text == "rtg" {
            rp.rtg_mem = attr(attrs, "memory").and_then(|m| m.parse().ok());
            continue;
        }
        if text == "fpu" {
            rp.fpu = attr(attrs, "type").map(str::to_string);
            continue;
        }
        match attr(attrs, "type") {
            Some("dd") => {
                if let Some(u) = attr(attrs, "unit").and_then(|u| u.parse::<usize>().ok()) {
                    rp.extra_drives = rp.extra_drives.max(u);
                }
            }
            Some(t) if t.starts_with("680") && t != "68000" && t != "68881" && t != "68882" => {
                rp.cpu = Some(t.to_string());
                rp.cpu_max |= attr(attrs, "speed") == Some("max");
            }
            _ => {}
        }
    }
    let mut floppies: Vec<(u32, String)> = tag_text(xml, "floppy")
        .into_iter()
        .map(|(a, t)| (attr(a, "priority").and_then(|p| p.parse().ok()).unwrap_or(99), t.to_string()))
        .collect();
    floppies.sort_by_key(|f| f.0);
    rp.floppy_names = floppies.into_iter().map(|(_, n)| n).collect();
    // Entries without a file name are the user's own shared folder
    // (root="shared"), which is not supported. root="data" entries are
    // folders that ship with Amiga Forever (Shared\dir\...).
    let host = |a: &str| attr(a, "root") == Some("data");
    let mut dirs: Vec<(u32, String, String)> = tag_text(xml, "harddrive")
        .into_iter()
        .filter(|(a, t)| host(a) && !t.is_empty())
        .map(|(a, t)| {
            let folder = t.rsplit(['\\', '/']).next().unwrap_or(t).to_string();
            let vol = attr(a, "volumename").map(str::to_string).unwrap_or_else(|| folder.clone());
            (attr(a, "priority").and_then(|p| p.parse().ok()).unwrap_or(99), folder, vol)
        })
        .collect();
    dirs.sort_by_key(|d| d.0);
    // Amiga Forever runs AROS with plenty of RAM and an RTG card. Without RTG
    // AROS remaps true-colour images for AGA, and with only the A4000's 4 MB of
    // motherboard RAM its stacks end up in slow chip RAM.
    if rp.system == "a-aros" && rp.fast_ram == 0 && rp.z3_ram == 0 {
        rp.fast_ram = 16 << 20;
        rp.z3_ram = 64 << 20;
    }
    // <boot type="dir">3x0</boot>: the shared OS 3.x System folder boots first.
    rp.boot_dir = tag_text(xml, "boot").into_iter().find(|(a, _)| attr(a, "type") == Some("dir")).map(|(_, t)| t.to_string());
    if rp.boot_dir.is_some() {
        dirs.insert(0, (0, "System".into(), "System".into()));
    }
    rp.rom = tag_text(xml, "rom").first().map(|(_, t)| t.to_string()).filter(|t| !t.is_empty());
    rp.host_dirs = dirs.into_iter().map(|(_, f, v)| (f, v)).collect();
    let mut hds: Vec<(u32, String)> = tag_text(xml, "harddrive")
        .into_iter()
        .filter(|(a, t)| !host(a) && !t.is_empty())
        .map(|(a, t)| (attr(a, "priority").and_then(|p| p.parse().ok()).unwrap_or(99), t.to_string()))
        .collect();
    hds.sort_by_key(|h| h.0);
    rp.harddrives = hds.into_iter().map(|(_, n)| n).collect();
    rp.boot_hdf = tag_text(xml, "boot")
        .into_iter()
        .find(|(a, _)| attr(a, "type") == Some("hdf"))
        .map(|(_, t)| t.to_string());
    rp.boot_adf = tag_text(xml, "boot")
        .into_iter()
        .find(|(a, _)| attr(a, "type") == Some("adf"))
        .map(|(_, t)| t.to_string());
    rp
}

pub fn load(path: &Path) -> Result<Rp9, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("not a valid RP9 (zip): {e}"))?;
    let xml = manifest_text(&mut zip)?;
    let mut rp = parse_manifest(&xml);
    for name in rp.floppy_names.clone() {
        let mut data = vec![];
        zip.by_name(&name)
            .map_err(|_| format!("RP9 is missing {name}"))?
            .read_to_end(&mut data)
            .map_err(|e| e.to_string())?;
        rp.floppies.push((name, data));
    }
    for name in rp.harddrives.clone() {
        let mut data = vec![];
        zip.by_name(&name)
            .map_err(|_| format!("RP9 is missing {name}"))?
            .read_to_end(&mut data)
            .map_err(|e| e.to_string())?;
        rp.hdfs.push((name, data));
    }
    Ok(rp)
}

impl Rp9 {
    /// Why this package cannot run yet, if it cannot.
    pub fn unsupported_reason(&self) -> Option<String> {
        match self.model() {
            None => return Some(format!("unknown system {}", self.system)),
            // Without a disc the CD32 shows its own start-up animation.
            Some(Model::Cd32 | Model::Cdtv) => return None,
            Some(m) if !m.supported() => return Some(format!("{m:?} is not emulated yet")),
            _ => {}
        }
        // A requested 68030/68040 is not a hard requirement: many such titles
        // run on the emulated 68020, and compat.tsv records which do not.
        if self.floppy_names.is_empty()
            && self.harddrives.is_empty()
            && self.host_dirs.is_empty()
            && self.boot_hdf.is_none()
            && self.boot_adf.is_none()
        {
            return Some("no Amiga disk media (Amiga Forever video, gallery or CD package)".into());
        }
        None
    }
}
