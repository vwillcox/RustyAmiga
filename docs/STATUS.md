# Status and known issues

## Running

- `./target/release/amiga` with no arguments opens the graphical launcher.
- `./target/release/amiga game.rp9` or `./target/release/amiga disk.adf` runs a title directly.
- Headless test options are listed by `--help`. They include `--headless`, `--screenshot`,
  `--shot-every`, `--wav`, `--type FRAME:TEXT` and `--debug-dump`.
- Debugging environment variables: `BREAK_AT`, `TRACE_AT`, `DISASM_AT`/`DISASM_N`,
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

## Known issues and open items

- **Powerball (1997): new regression.** It showed a picture before tonight's batch of changes and is blank after them. The cause is most likely the prefetch timing change or the POT counters. Bisect this first next session.
- **Voyage (Razor 1911): protection fails.** It uses a trace-mode decoder that modifies its handler's own code. Prefetch emulation helped, but the decrypted check still fails and the demo resets. The next suspect is the key in d6, which may be derived from timing.
- **Tie Break works but is slow.** Its trackloader takes about 100 s at authentic disk speed, so the 90 s automated test marked it blank. Use warp (End).
- **Rink-a-Dink works.** It waits for a left mouse click after the intro, so the automated test showed black.
- **AROS:** the extension ROM is mapped, but the screen stays dark. Not yet diagnosed.
- **Logica DiagROM:** shows a garbage display. Not yet diagnosed.
- **Not implemented:**
  - BPLCON2 undocumented priority codes 5-7.
  - The exact BLTBDAT hold semantics.
  - Cycle-exact blitter and CPU timing. Blits are immediate, with timed completion.
  - Bus and address error exceptions.
  - HDF hard disks (102 Amiga Forever titles).
  - 68020/030/040, FPU and the AGA chipset (A1200/A3000/A4000/CD32 Kickstarts and 20 titles).

## Next milestones

1. Fix the Powerball regression.
2. HDF hard-disk support.
3. 68020 CPU and the AGA chipset.
