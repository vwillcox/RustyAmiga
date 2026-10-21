# Status and known issues

## Running

- `./target/release/amiga` with no arguments opens the graphical launcher.
- `./target/release/amiga game.rp9` or `./target/release/amiga disk.adf` runs a title directly.
- `--hdf image.hdf` (repeatable) attaches hard disks to an A590/A2091 SCSI controller, SCSI IDs 0 up.
  RP9 hard disk titles also need the shared Amiga Forever boot disks (`workbench-135.hdf` etc. from
  `Amiga Files/Shared/hdf` on the AF DVD) in `./hdf`.
  Packages built on host folders (AROS, Amiga 4000 Enhanced 3.X) need `Amiga Files/Shared/dir` in `./dir`.
- Headless test options are listed by `--help`. They include `--headless`, `--screenshot`,
  `--shot-every`, `--wav`, `--type FRAME:TEXT` and `--debug-dump`.
- Debugging environment variables: `BREAK_AT`, `TRACE_AT`, `TRACE_RANGE=lo-hi` (log every instruction in a
  range), `DISASM_AT`/`DISASM_N`,
  `AMIGA_LOG_EXCEPTIONS`, `AMIGA_LOG_DISK`.

## Fixed in the 2026-10-19 session (chipset audit)

| Area | Problem | Fix |
|---|---|---|
| Blitter | D-channel writes happened immediately. Overlapping blits corrupted their own sources, which caused fill artefacts in 3D Demo (Anarchy). | D writes are pipelined one word behind the source reads, as on hardware. |
| Blitter | Line mode wrote even with channel C disabled. | Line mode only writes when C is enabled. |
| CPU | There was no prefetch queue. Self-modifying code and trace decoders saw modified instructions that real hardware ignores. | Added a two-word 68000 prefetch queue. The SingleStepTests suite still passes. |
| Denise | BPU=7 was treated as 4 planes. | The undocumented 7-plane trick works: 4 planes come from DMA and Denise displays 6, with planes 5 and 6 taken from BPL5DAT/BPL6DAT. |
| Paula | POT0DAT/POT1DAT were stubbed. | POTGO START resets the counters, which then count lines. |
| ECS | BPLCON3, BEAMCON0, DIWHIGH and DENISEID were missing. | Border blanking, PAL/NTSC switching, extended display-window bits and the ECS Denise ID ($FC) are implemented. |
| Agnus | A DDFSTRT value above $D8 panicked. | Clamped. |
| Memory | Kickstart 1.2 mis-sized slow RAM. | Unpopulated $C00000 space mirrors the custom chips. |
| Audio | The output filter was a generic low-pass. | Models the A500/A1200 RC filter, the 2-pole LED filter and the DC-blocking high-pass, run at the colour-clock rate. |

## Hard disks (2026-10-20)

The A590/A2091 controller is emulated at the hardware level and driven by Commodore's own boot ROM
(`amiga-boot-a590.rom`), which does the RDB parsing, filesystem loading and autobooting.

- The DMAC registers are emulated, with the ROM mapped from board offset $2000. The A590 XT registers read as $FF, meaning no XT drive is fitted.
- The WD33C93 is the original non-A chip. It tracks SCSI bus phases and supports both polled and DMA Transfer Info, Select-and-Transfer (including resume from the Command Phase register), and the disconnect interrupt that follows Select-and-Transfer, which Commodore's driver waits for.
- HDF images are served as SCSI disk targets. Writes stay in memory and are never saved.
- Amiga Forever's boot disks set the RDB "last disk" flags. The real driver honours them and would stop scanning after unit 0, so they are cleared in memory on every disk except the last one, as HDToolBox would do.
- All 19 Kickstart 1.3 hard disk titles (A2000/A500+) run. The other 83 hard disk titles need 68020+/AGA.

## 68020-68040, FPU, AGA, A1200 and A4000 (2026-10-20)

- CPUs: 68020, 68030 (MMU registers only) and 68040 (MOVEC MMU/TT registers, MOVE16, CINV/CPUSH; no translation). FPU: 68881/68882/68040, computed in f64. Unit tests are in `tests/cpu020.rs`.
- The `--cpu-speed` multiplier scales non-chip-bus CPU time. Defaults: A1200 2x, A4000 4x, RP9 `speed="max"` 8x.
- AGA (Lisa/Alice): 24-bit palette with bank select and LOCT, 8 planes, HAM8, BPLCON4, FMODE 1x/2x/4x, wide sprites.
- A1200: Gayle IDE. A4000: 32-bit address space, motherboard and Zorro III RAM, A4000 IDE, Ramsey.
- Amiga Forever's bare-partition application HDFs get an RDB synthesized in memory (`src/hdf.rs`).
- Shared boot media come from the AF DVD: `./hdf/workbench-*.hdf` and `./adf/amiga-os-*-workbench.adf`.
- The SingleStepTests data lives in `./sst` (gitignored). Run it with `SST_DIR=sst/v1 cargo test --release --test sst -- --nocapture`. The whole suite passes (261894 cases; address-error cases are skipped).

### Fixed after the first AGA pass
- **Lisa's DENISEID now reads `$00F8`.** Its upper byte is the revision; with `$FF` there, graphics.library built an ECS-limited display database and 8-plane OS screens failed to open. This fixed about a dozen OS-friendly AGA demos.
- **An empty IDE port reads `$FF`.** Previously scsi.device retried its probe 30 times, which delayed every A1200 floppy boot.

### A3000
ECS with a 68030 and 68882, a 32-bit address space and Ramsey. The built-in SCSI is the SDMAC at `$DD0000` with a WD33C93A, driven by Kickstart's scsi.device. The A2091 code is reused: register offsets are translated, and CNTR bits sit two places lower than on the A2091. SASR is also decoded at `$48`.

### HD floppies
Drives report their ID on /RDY while the motor is off (disk.resource reads a 1 where /RDY is high): DD drives read `$00000000`, and HD drives holding an HD disk read `$AAAAAAAA`. Inserting a 1.76 MB image makes the drive an HD drive. HD tracks are twice as long, so the 150 RPM rotation comes out naturally.

### Front-panel lights
The window shows power, per-drive floppy (green: motor on; blinking yellow: disk DMA running) and hard disk (amber: busy) lights under the picture. `--leds` adds them to headless screenshots.

### Display window starting at line 0
Aerial Racers, AmiBee and Overdose set DIWSTRT to `$0081`, a vertical start of line 0. Agnus closed the window at line 0 after opening it, so their bitplanes never showed. Real Agnus (except the A1000's) opens the window at once.

Paula also now finishes the current word when a channel's DMA is switched off, then requests an interrupt unless one is pending, as in the HRM state diagram. Sound drivers wait for that interrupt after stopping a channel.

### JIT and maximum-speed packages
59 RP9s ask for UAE's JIT and 12 more for `speed="max"` (UAE's "fastest possible" CPU), so the demos expect far more CPU than a real 68030/040. Those packages now get an adaptive CPU speed in the window: every 25 frames the multiplier rises by 25% while emulation uses under 60% of the frame time, and drops by 20% above 85%. It never goes below the model default and is shown in the title bar. Headless runs use a fixed 32x so test results are repeatable. `--cpu-speed` overrides both. This fixed Concrete, Lux Aeterna Luceat Eis, Psylteflesk, Tour de Scene, The Chronicles of Professor Arronax, Multiverse and Lapsuus, which only looked blank because they were still precalculating, and The Castle.

### DIWHIGH horizontal bits and BSCAN2
- Denise now takes the horizontal H8 bits from DIWHIGH. A stop position left of the start (Mental's `$00A`) keeps the window open to the end of the line, because the window is a flip-flop.
- AGA BSCAN2 (FMODE bit 14) scan doubling: every plane adds BPL1MOD and BPL2MOD on alternate lines, with the parity counted from the DIWSTRT line. Mental, Goa, Lapsuus, Ocean Machine, Burning Chrome, Under Pressure and Sumea use it and showed vertical streaks or garbage. Fatman's title picture is no longer cut off.

### Host folders as hard disks
Amiga Forever ships AROS and the enhanced OS 3.X installation as plain folders (`Amiga Files/Shared/dir` on the DVD: `AROS`, `System` and `Work`), which UAE mounts with its own filesystem handler. Put them in `./dir`. When the machine starts, each folder is built into an international FFS (`DOS\3`) partition image in memory (`src/dirfs.rs`), which Kickstart's FFS mounts like any other disk. `--dir-hd FOLDER` does the same for any folder.
- Writes stay in memory. Names longer than 30 characters are shortened, and host names are converted to Latin-1. There are no Amiga protection bits on the host, so files in S: and text files that start like scripts get the S bit.
- **Amiga 4000 Enhanced 3.X** boots the 3.X ROM from System with Work as DH1. It first shows "uaegfx.card could not be opened" (no RTG card); after OK (`--type 1000:\Av` headless), Workbench comes up on AGA.
- **AROS** boots from its folder to Wanderer on an A4000 given 16 MB motherboard and 64 MB Zorro III RAM. With the default 4 MB, its stacks ended up in chip RAM, and remapping its true-colour images for AGA took minutes.

### Remaining open items
- **Incision still precalculates at 90 s.** It renders its soundtrack first; at 64x this finishes around frame 7500.
- **Not emulated:** CD32/CDTV, PowerPC boards, UAE's RTG card (uaegfx), and the user's own shared folder (`<harddrive root="shared"/>`, which is skipped). The Walker prototype's folders are not on the AF 11 DVD.
- **Not Amiga software:** the Videos and Gallery packages (DVD video and host images for Amiga Forever's player). The emulator now refuses them with that explanation.

## Known issues and open items

- **Powerball works (not a regression).** Its intro sequence includes black gaps, and the menu only appears at about frame 7000 (140 s), or frame 5750 with turbo floppy. The 90 s single-frame check landed on a black gap.
- **Voyage (Razor 1911): protection fails.** It uses a trace-mode decoder that modifies its handler's own code. Prefetch emulation helped, but the decrypted check still fails and the demo resets. The next suspect is the key in d6, which may be derived from timing.
- **Tie Break works but is slow.** Its trackloader takes about 100 s at authentic disk speed, so the 90 s automated test marked it blank. Use warp (End).
- **Rink-a-Dink works.** It waits for a left mouse click after the intro, so the automated test showed black.
- **Logica Dialoga 2.0 (diagnostic ROM) works.** Its colour-bar test pattern is followed by the title screen. It continues on a mouse button or fire button; the keyboard is only enabled after its keyboard test.
- **Not implemented:**
  - BPLCON2 undocumented priority codes 5-7.
  - The exact BLTBDAT hold semantics.
  - Cycle-exact blitter and CPU timing. Blits are immediate, with timed completion.
  - Bus and address error exceptions.
  - 68030/68040 MMU address translation, and the 68060.

## Next milestones

1. A3000 polish (the SDMAC works; nothing beyond booting is tested).
2. The remaining blank AGA titles; CD32/CDTV.
