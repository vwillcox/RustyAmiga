//! MPEG-1 decoding for the CD32 FMV cartridge: video (ISO 11172-2, done on
//! the cartridge by the C-Cube CL450) and Layer II audio (ISO 11172-3, the
//! LSI Logic L64111).

pub mod audio;
mod bits;
pub mod video;
mod vlc;

pub use bits::Bits;
