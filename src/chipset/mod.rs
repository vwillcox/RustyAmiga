pub mod audio;
pub mod blitter;
pub mod denise;

/// INTREQ / INTENA bits.
pub mod int {
    pub const TBE: u16 = 1 << 0;
    pub const DSKBLK: u16 = 1 << 1;
    pub const SOFT: u16 = 1 << 2;
    pub const PORTS: u16 = 1 << 3;
    pub const COPER: u16 = 1 << 4;
    pub const VERTB: u16 = 1 << 5;
    pub const BLIT: u16 = 1 << 6;
    pub const AUD0: u16 = 1 << 7;
    pub const RBF: u16 = 1 << 11;
    pub const DSKSYN: u16 = 1 << 12;
    pub const EXTER: u16 = 1 << 13;
    pub const INTEN: u16 = 1 << 14;
}

/// DMACON bits.
pub mod dma {
    pub const AUD: u16 = 0x000f;
    pub const DSK: u16 = 1 << 4;
    pub const SPR: u16 = 1 << 5;
    pub const BLT: u16 = 1 << 6;
    pub const COP: u16 = 1 << 7;
    pub const BPL: u16 = 1 << 8;
    pub const MASTER: u16 = 1 << 9;
    pub const BLTPRI: u16 = 1 << 10;
}
