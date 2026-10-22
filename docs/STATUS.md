# Status and known issues

## Running

- `./target/release/amiga` with no arguments opens the graphical launcher.
- `tools/install-desktop.sh` adds it to the desktop's application menu (Omarchy, GNOME, KDE...) with an icon; the entry starts in the checkout so the ROM and media folders are found.
- `./target/release/amiga game.rp9` or `./target/release/amiga disk.adf` runs a title directly.
- `--hdf image.hdf` (repeatable) attaches hard disks, SCSI IDs 0 up: to a disk controller chosen with `--board` (such as `a4091`), else the model's own (A3000 and A4000T SCSI, A600/A1200/A4000 IDE), else an A590/A2091 that is added for them.
- `--cd image.iso` (or a `.cue` sheet with its BIN files) puts the disc in the CD32's drive. On other models it puts a SCSI CD-ROM drive at the next free SCSI ID on the same controller; a machine without SCSI gets an ATAPI drive on its IDE port (A600/A1200/A4000), or else an A590/A2091.
- `--joy FRAME:BUTTONS` (headless) holds joystick buttons for 10 frames, e.g. `1100:fire` or `900:left+fire`.
  RP9 hard disk titles also need the shared Amiga Forever boot disks (`workbench-135.hdf` etc. from
  `Amiga Files/Shared/hdf` on the AF DVD) in `./hdf`.
  Packages built on host folders (AROS, Amiga 4000 Enhanced 3.X) need `Amiga Files/Shared/dir` in `./dir`.
- Headless test options are listed by `--help`. They include `--headless`, `--screenshot`,
  `--shot-every`, `--wav`, `--type FRAME:TEXT` and `--debug-dump`.
- Debugging environment variables: `BREAK_AT` (with `BREAK_MIN_FRAME` to skip earlier hits), `TRACE_AT`,
  `TRACE_RANGE=lo-hi` (log every instruction in a range, with beam position and cycle count; `TRACE_FRAMES=a-b`
  limits it to those frames), `DISASM_AT`/`DISASM_N`, `DUMP_MEM=addr:len`, `AMIGA_LOG_EXCEPTIONS`, `AMIGA_LOG_DISK`.

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
- The SingleStepTests data lives in `./sst` (gitignored). Run it with `SST_DIR=sst/v1 cargo test --release --test sst -- --nocapture`. The whole suite passes (261894 cases). Address-error cases are skipped by default; `SST_ADDR_ERR=1` includes them and `SST_ADDR_ERR=only` runs just those (54773 of 55606 pass).

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

### CD32
The CD32 model has Akiko at `$B80000` (`src/akiko.rs`): the chip ID, the chunky-to-planar converter, and the I2C settings NVRAM (a 24C08, kept in memory). The extended ROM loads automatically. Without a disc the machine boots to its "Amiga CD32" disc animation.

The CD drive (`src/akiko_cd.rs`, after WinUAE's akiko.cpp) talks to the ROM over Akiko's DMA rings. It supports the status/firmware, TOC, data read (raw sectors into the PBX slots), CD audio play/pause/stop, Q subchannel and LED commands. Discs are ISO files or CUE/BIN sheets with audio tracks (`src/cdimage.rs`). CD audio is mixed with Paula's output. CIA-A PA0 mutes it, because the CD32 has no ROM overlay bit; treating PA0 as the overlay made the CD player crash when it unmuted.
- Amiga CD32 Gamer Vol. 12 (archive.org, CUE/BIN) boots to its menu.
- Its audio tracks, in a cue sheet without the data track, play in the ROM's CD player, which shows the track and the running time.
- Not yet emulated: the CD32 joypad's extra buttons, CD+G and audio scanning.

### 68000 bus timing (Voyage)
Voyage (Razor 1911) now runs. Its protection needed three timing details:
- **Prefetch.** The 68000 refills the second prefetch word at the end of an instruction, after the instruction's own writes. Voyage turns on trace mode with an `ORI #$A01C,SR` whose immediate it writes from the instruction just before. A taken branch also no longer prefetches past its displacement, and DBcc takes its 10 cycles.
- **Chip bus.** CPU accesses start on any colour clock, not only even ones. The protection builds its decryption key from where two CIA timer interrupts fall in a copy loop, and only the 58-cycle loop that this gives decrypts. The key was found by modelling the key derivation and testing candidate keys.
- **Interrupt latency.** A newly raised interrupt level reaches the CPU 3 colour clocks after Paula raises it. F1 GP Circuits polls VPOSR for line 0, where the vertical blank interrupt is raised, and hung without the delay. Voyage also needs 3 or 4.

### 68000 address errors
A word or long access at an odd address, or a jump to one, raises a group 0 exception with the 14-byte frame (status word, access address, IR, SR, PC). The instruction stops at the faulting access. Where its address registers, flags and stacked PC end up follows the SingleStepTests data. The misses (1.5%) are in MOVE's partial flag updates and a few stacked PCs. The emulator used to carry on silently, so crashing programs now Guru with 80000003 as on real hardware. The RP9 batch is unchanged.

### Cycle-exact 68000
The 68000 core matches the SingleStepTests suite's cycle count for every one of its 261894 cases (the harness now checks them): exception timing, branch prefetch behaviour, CCR/SR immediates, LEA/PEA indexed modes, bit operations, CHK, STOP, and DIVU/DIVS with the operand-dependent timings of the microcode. Write-only and unused custom registers now read back the last chip bus value (Dr. Mario relies on it).

### Drive noises
Floppy motor whirr, head steps (loading chugs; an empty drive clicks) and hard disk seeks, synthesized in `src/drivesound.rs` and mixed after Paula. Pause toggles them; `--no-drive-sounds` turns them off.

### Expansion boards and clocks
Boards sit on one autoconfig chain (`src/expansion/`): Zorro II boards answer at `$E80000`, Zorro III boards at `$FF000000`, in slot order. `--board ID[:options]` adds a board from the registry (`--list-boards`), and the launcher has an Expansion boards section. The generic fast RAM options (`--fast`, `--z3`, RP9 RAM) and the A590/A2091 use the same chain. The plan for the rest of the boards is in `docs/ADDONS.md`.
- RAM boards with their real IDs: A2052, A2058, A560, Supra 500RX/2000, MicroBotics 8-Up, GVP Impact Series II, Oktagon 2008, MBX 1200z; the A2091 can carry its own RAM (`a2091:ram=2M`).
- Battery-backed clocks at `$DC0000`: OKI MSM6242B on the A2000 and A500+ and on the A501, A601 and MBX 1200z trapdoor boards; Ricoh RF5C01A on the A3000/A4000. They run from the host's local time; setting the date from AmigaOS holds for the session.

### NCR 53C710: A4091 and A4000T
The 53C710 runs SCRIPTS, its own programs fetched from memory by DMA (`src/ncr710.rs`): block moves (direct, indirect, table indirect), SELECT (also table indirect), WAIT DISCONNECT/RESELECT, SET/CLEAR ATN and ACK, jumps, calls, returns and INT on phase and data conditions with relative addressing, register arithmetic, and memory moves, including those that target the chip's own registers. The targets' side of the bus (`src/scsi.rs`) handles messages: IDENTIFY, synchronous and wide negotiation (answered as asynchronous, 8-bit), MESSAGE REJECT and ABORT. ATN and the ACK held after MESSAGE IN are modelled. Missing targets time out after 1 ms.
- `--board a4091` (A3000/A4000): Commodore's Zorro III controller with its own boot ROM (`amiga-boot-a4091.rom`): the nibble-wide ROM, the chip at `$800000` (and its `$840000` alias) and the DIP switches at `$8C0003`. Workbench 3.1 boots from it.
- `--model a4000t`: the tower A4000, with the chip on the motherboard at `$DD0040` (mirrored at `$DD00C0`) driven by the A4000T Kickstart's scsi.device. Its IDE port is the A4000's. Workbench 3.1 boots from its SCSI disk.
- SCSI CD-ROM drive (`--cd`): an ISO 9660 image read from the file as needed, with 2048-byte blocks, INQUIRY type 5 (removable), READ(6/10/12), READ CAPACITY, READ TOC (LBA and MSF), READ SUB-CHANNEL and MODE SENSE (write-protected). With the Amiga Forever System folder's CacheCDFS mounted on scsi.device unit 1, the Amiga Forever DVD image mounts as a 1614 MB volume on the A4091 and on the A3000's SCSI.
- I/O boards share one interface (`Controller` in `src/expansion/`): registers, boot ROM, DMA, interrupt, reset and disks. The next storage chips (53C9x, 5380) will plug in the same way.

### ATAPI CD-ROM (A600, A1200, A4000)
On an IDE port the CD-ROM drive is an ATAPI device (`src/gayle.rs`). It answers IDENTIFY DEVICE with an abort and the $EB14 signature, IDENTIFY PACKET DEVICE, and PACKET: the host writes a 12-byte command, the drive executes it with the SCSI CD-ROM code (`src/scsi.rs`, which also has MODE SENSE/SELECT(10) for ATAPI drivers), and the data goes back in chunks no larger than the byte count limit, with the interrupt reason in the sector count register. Errors set CHK with the sense key in the error register.
- The scsi.device of the Kickstart 3.X ROMs (43.46) finds the drive, and CacheCDFS mounts the Amiga Forever DVD on the A600, A1200 and A4000. Kickstart 3.1's scsi.device has no ATAPI support (like the real ROM): the machine boots, but the drive cannot be used.

### PCMCIA memory cards (A600, A1200)
`--pcmcia image` puts an SRAM card (up to 4 MB) in Gayle's slot: common memory at `$600000`, attribute memory (the card's CIS, one byte per even address) at `$A00000`. A folder becomes an FFS-formatted card, the smallest of 512 KB, 1, 2 or 4 MB that holds it. Writes stay in memory.
- Insertion sets Gayle's card-detect bit in the status latch and raises INT6, as the hardware does. (Raising INT2 instead made Kickstart 2.05 hang at a white screen.)
- A blank card (no CISTPL_FORMAT tuple) is added as Fast RAM by Kickstart 2.05 and 3.x. A card made from a folder carries CISTPL_FORMAT and CISTPL_GEOMETRY tuples, and carddisk.device mounts it as `CC0:`.

### Remaining open items
- **Incision works but precalculates for a long time.** It renders its soundtrack first; at 64x the intro starts by frame 9000.
- **Not emulated:** the CDTV, PowerPC boards, UAE's RTG card (uaegfx), and the user's own shared folder (`<harddrive root="shared"/>`, which is skipped). The Walker prototype's folders are not on the AF 11 DVD.
- **Not Amiga software:** the Videos and Gallery packages (DVD video and host images for Amiga Forever's player). The emulator now refuses them with that explanation.

## Known issues and open items

- **Powerball works (not a regression).** Its intro sequence includes black gaps, and the menu only appears at about frame 7000 (140 s), or frame 5750 with turbo floppy. The 90 s single-frame check landed on a black gap.
- **Tie Break works but is slow.** Its trackloader takes about 100 s at authentic disk speed, so the 90 s automated test marked it blank. Use warp (End).
- **Rink-a-Dink works.** It waits for a left mouse click after the intro, so the automated test showed black.
- **Logica Dialoga 2.0 (diagnostic ROM) works.** Its colour-bar test pattern is followed by the title screen. It continues on a mouse button or fire button; the keyboard is only enabled after its keyboard test.
- **Not implemented:**
  - BPLCON2 undocumented priority codes 5-7.
  - The exact BLTBDAT hold semantics.
  - Cycle-exact blitter. Blits are immediate, with timed completion. (The 68000 is cycle-exact; the 68020+ are not.)
  - Bus error exceptions (address errors are emulated).
  - 68030/68040 MMU address translation, and the 68060.

## Next milestones

1. A3000 polish (the SDMAC works; nothing beyond booting is tested).
2. The CD32 joypad, then the CDTV (its CD drive and controller).
