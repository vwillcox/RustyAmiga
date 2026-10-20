/// Interface between the CPU and the rest of the machine.
///
/// All timing lives on the bus side: every access advances the machine clock
/// by one bus cycle (4 CPU clocks plus any wait states) and `idle` accounts for
/// internal CPU processing time.
pub trait Bus {
    fn read8(&mut self, addr: u32) -> u8;
    fn read16(&mut self, addr: u32) -> u16;
    fn write8(&mut self, addr: u32, value: u8);
    fn write16(&mut self, addr: u32, value: u16);

    /// Internal CPU cycles that do not touch the bus.
    fn idle(&mut self, _cycles: u32) {}

    /// Current interrupt priority level presented on the IPL lines (0-7).
    fn ipl(&mut self) -> u8 {
        0
    }

    /// The RESET instruction asserts the reset line for external devices.
    fn reset_devices(&mut self) {}
}
