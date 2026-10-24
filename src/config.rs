#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum, serde::Serialize, serde::Deserialize)]
pub enum Model {
    A500,
    A500Plus,
    A600,
    A1000,
    A2000,
    A1200,
    A3000,
    A4000,
    /// The tower A4000: an A4000 with a built-in NCR 53C710 SCSI controller.
    #[value(name = "a4000t")]
    A4000T,
    Cd32,
    /// The CDTV: an A500 (1 MB chip RAM, Kickstart 1.3) with a CD drive.
    Cdtv,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chipset {
    Ocs,
    Ecs,
    Aga,
}

impl Model {
    pub fn chipset(self) -> Chipset {
        match self {
            Model::A500 | Model::A1000 | Model::A2000 | Model::Cdtv => Chipset::Ocs,
            Model::A500Plus | Model::A600 | Model::A3000 => Chipset::Ecs,
            Model::A1200 | Model::A4000 | Model::A4000T | Model::Cd32 => Chipset::Aga,
        }
    }

    /// Cut-off of the fixed RC low-pass filter on the audio output.
    pub fn audio_lowpass_hz(self) -> f64 {
        match self {
            // 360 ohm / 0.1 uF
            Model::A500 | Model::A500Plus | Model::A600 | Model::A2000 | Model::A1000 | Model::A3000 | Model::Cdtv => 4420.97,
            // 1.39 kohm / 3.3 nF
            _ => 34419.0,
        }
    }

    pub fn supported(self) -> bool {
        matches!(self, Model::A500 | Model::A1000 | Model::A2000 | Model::A500Plus | Model::A600 | Model::A1200 | Model::A3000 | Model::A4000 | Model::A4000T | Model::Cd32 | Model::Cdtv)
    }

    /// Machines with a 32-bit address bus (A3000/A4000).
    pub fn addr32(self) -> bool {
        matches!(self, Model::A3000 | Model::A4000 | Model::A4000T)
    }

    pub fn cpu(self) -> crate::cpu::CpuModel {
        match self {
            Model::A1200 | Model::Cd32 => crate::cpu::CpuModel::M68020,
            Model::A3000 => crate::cpu::CpuModel::M68030,
            Model::A4000 | Model::A4000T => crate::cpu::CpuModel::M68040,
            _ => crate::cpu::CpuModel::M68000,
        }
    }
}

/// A board from the registry with its options.
#[derive(Clone, Debug)]
pub struct BoardSpec {
    pub id: String,
    pub opts: crate::expansion::registry::Options,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub model: Model,
    pub chip_kb: u32,
    pub slow_kb: u32,
    pub fast_kb: u32,
    /// A3000/A4000 motherboard fast RAM (ends at $08000000).
    pub mb_kb: u32,
    /// Zorro III fast RAM (A3000/A4000).
    pub z3_kb: u32,
    /// Add-on boards (`--board`), in slot order.
    pub boards: Vec<BoardSpec>,
    /// Battery-backed clock at $DC0000.
    pub rtc: Option<crate::rtc::RtcKind>,
    pub cpu: crate::cpu::CpuModel,
    pub fpu: crate::cpu::FpuModel,
    /// CPU speed as a multiple of a 7 MHz 68000 (for non-chip-bus work).
    pub cpu_speed: f32,
    /// Raise `cpu_speed` while the host keeps up (windowed mode), like a JIT core.
    pub cpu_speed_adaptive: bool,
    pub ntsc: bool,
    pub turbo_floppy: bool,
    pub drives: usize,
}

impl Config {
    pub fn for_model(model: Model) -> Config {
        let (chip_kb, slow_kb) = match model {
            Model::A500 | Model::A2000 => (512, 512),
            Model::A1000 => (256, 0),
            Model::A500Plus | Model::A600 | Model::A3000 | Model::Cdtv => (1024, 0),
            _ => (2048, 0),
        };
        Config {
            model,
            chip_kb,
            slow_kb,
            fast_kb: 0,
            mb_kb: if model.addr32() { 4096 } else { 0 },
            z3_kb: 0,
            boards: vec![],
            // The A2000 and A500+ have a clock on the motherboard; the A3000 and
            // A4000 a Ricoh one. Others need a trapdoor board.
            rtc: match model {
                Model::A2000 | Model::A500Plus | Model::Cdtv => Some(crate::rtc::RtcKind::Msm6242),
                Model::A3000 | Model::A4000 | Model::A4000T => Some(crate::rtc::RtcKind::Rf5c01),
                _ => None,
            },
            cpu: model.cpu(),
            fpu: match model {
                Model::A3000 => crate::cpu::FpuModel::M68882,
                Model::A4000 | Model::A4000T => crate::cpu::FpuModel::M68040,
                _ => crate::cpu::FpuModel::None,
            },
            cpu_speed: match model {
                Model::A1200 | Model::Cd32 => 2.0,
                Model::A3000 | Model::A4000 | Model::A4000T => 4.0,
                _ => 1.0,
            },
            cpu_speed_adaptive: false,
            ntsc: false,
            turbo_floppy: false,
            // The CD32 and CDTV have no internal floppy drive.
            drives: if matches!(model, Model::Cd32 | Model::Cdtv) { 0 } else { 1 },
        }
    }
}
