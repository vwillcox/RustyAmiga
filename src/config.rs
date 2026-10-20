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
    Cd32,
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
            Model::A500 | Model::A1000 | Model::A2000 => Chipset::Ocs,
            Model::A500Plus | Model::A600 | Model::A3000 => Chipset::Ecs,
            Model::A1200 | Model::A4000 | Model::Cd32 => Chipset::Aga,
        }
    }

    /// Cut-off of the fixed RC low-pass filter on the audio output.
    pub fn audio_lowpass_hz(self) -> f64 {
        match self {
            // 360 ohm / 0.1 uF
            Model::A500 | Model::A500Plus | Model::A600 | Model::A2000 | Model::A1000 | Model::A3000 => 4420.97,
            // 1.39 kohm / 3.3 nF
            _ => 34419.0,
        }
    }

    pub fn supported(self) -> bool {
        matches!(self, Model::A500 | Model::A1000 | Model::A2000 | Model::A500Plus | Model::A600 | Model::A1200)
    }

    pub fn cpu(self) -> crate::cpu::CpuModel {
        match self {
            Model::A1200 | Model::A3000 | Model::A4000 | Model::Cd32 => crate::cpu::CpuModel::M68020,
            _ => crate::cpu::CpuModel::M68000,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub model: Model,
    pub chip_kb: u32,
    pub slow_kb: u32,
    pub fast_kb: u32,
    pub ntsc: bool,
    pub turbo_floppy: bool,
    pub drives: usize,
}

impl Config {
    pub fn for_model(model: Model) -> Config {
        let (chip_kb, slow_kb) = match model {
            Model::A500 | Model::A2000 => (512, 512),
            Model::A1000 => (256, 0),
            Model::A500Plus | Model::A600 | Model::A3000 => (1024, 0),
            _ => (2048, 0),
        };
        Config {
            model,
            chip_kb,
            slow_kb,
            fast_kb: 0,
            ntsc: false,
            turbo_floppy: false,
            drives: 1,
        }
    }
}
