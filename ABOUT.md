# About RustyAmiga

RustyAmiga is an Amiga emulator in Rust, built in stages: first an A500 with OCS, then the 68020 to
68040 and AGA machines, then storage controllers and expansion boards. This document explains how
it is put together, and some of the problems that took the most work to solve.

## Principles

**Emulate the hardware, not the software.** Many emulators speed things up by patching the
operating system: a virtual hard disk device, a host filesystem handler, or a fake RTG card.
RustyAmiga emulates the chips instead, and lets Commodore's ROMs and drivers do their own work. The
A590 hard disk controller is driven by Commodore's boot ROM. The A4091 SCSI card runs its own driver,
which uploads programs to the SCSI chip. PCMCIA cards are recognized by Kickstart's card.resource.
That costs more effort, but it means real software sees real hardware, and its own quirks work as
they did on a real machine. The one exception is UAE's virtual graphics card, which Amiga Forever's
OS 3.X system is set up for (see below).

**Test against real software.** The regression suite is the Amiga Forever collection: around 240
games, demos and system packages in `.rp9` form. Every change is run against all of them with the
old and new binaries, headless and in parallel, saving screenshots at fixed frames. The
battery-backed clock is pinned to the same time for every run, as the time of day changes how long
booting takes and so the timing of everything after it. Titles whose pictures change are put side by
side on a contact sheet and checked by eye. Judging titles by several frames, not only the last one,
avoids mistaking a demo's black gap between scenes for a failure.

**Test the CPU against the reference.** The 68000 core is checked against the SingleStepTests suite
(261,894 cases), including the exact cycle count of each one.

## Architecture

| Part | Where | What it does |
|---|---|---|
| CPU | `src/cpu/` | 68000-68040 interpreter, prefetch queue, exceptions, FPU (computed in f64) |
| Chipset | `src/chipset/`, `src/board.rs` | Agnus/Alice (DMA, copper, blitter), Denise/Lisa (display), Paula (audio, disk, interrupts) |
| Bus and timing | `src/board.rs`, `src/bus.rs` | Every CPU access advances the machine clock; chip RAM accesses wait for free DMA slots |
| CIAs, keyboard, RTC | `src/cia.rs`, `src/keyboard.rs`, `src/rtc.rs` | Timers, time of day, ports, the battery-backed clocks |
| Floppy | `src/floppy.rs` | MFM-encoded tracks generated from ADF images; DD and HD drives |
| Storage | `src/a2091.rs`, `src/gayle.rs`, `src/ncr710.rs`, `src/scsi.rs` | WD33C93, IDE, NCR 53C710, SCSI targets, PCMCIA |
| Expansion | `src/expansion/` | Autoconfig chain, board registry, the `Controller` trait for I/O boards |
| Media | `src/rp9.rs`, `src/hdf.rs`, `src/dirfs.rs`, `src/rom.rs` | RP9 packages, RDB synthesis, folders built into FFS volumes, encrypted ROMs |
| Front end | `src/frontend.rs`, `src/launcher.rs` | Window, audio, input, the egui launcher |

The machine runs as one loop driven by the colour clock (3.55 MHz on PAL). The CPU runs between
chip-bus slots, so DMA contention and copper/blitter timing fall out of the same model, instead of
being approximated separately.

## Challenges and solutions

### A cycle-exact 68000
Demos and copy protections often measure time with the CPU itself. The 68000 core had to match
the real chip not just in results but in timing. That covers the two-word prefetch queue, refilled
after an instruction's own writes; branch prefetch behaviour; exception stacking; and the
operand-dependent timings of DIVU/DIVS, which follow the microcode's steps. The SingleStepTests
harness now checks cycle counts, and every case matches. Address errors raise the real 14-byte
exception frame, so crashing programs show a Guru Meditation instead of carrying on silently.

### Voyage's copy protection
Razor 1911's *Voyage* builds a decryption key from where two CIA timer interrupts fall inside a copy
loop. Three timing details had to be exactly right:
- the prefetch refill order, because the demo enables trace mode with an instruction whose
  immediate operand it has just written;
- CPU chip-bus accesses starting on any colour clock, not only even ones;
- a 3-colour-clock delay between Paula raising an interrupt and the CPU seeing it.

The right loop timing was found by modelling the key derivation and testing candidate keys
against the encrypted data.

### The blitter overwriting its own source
In overlapping blits, the D channel writes one word behind the source reads. Writing immediately
corrupted the data still to be read, which caused fill artefacts in *3D Demo*. The fix pipelines D
writes as the hardware does.

### AGA's identity crisis
With `$FF` in the high byte of Lisa's DENISEID, graphics.library decided the chipset was ECS,
and 8-plane screens failed to open. Reading `$00F8`, as real Lisa does, fixed about a dozen AGA demos
at once. Other AGA work included BSCAN2 scan doubling (alternating plane modulos) and the DIWHIGH
horizontal bits. In DIWHIGH, a stop position left of the start keeps the window open, because the
display window is a flip-flop, not a range.

### Hard disks through Commodore's own drivers
Instead of a virtual device, the A590/A2091 is emulated as its DMA controller and a WD33C93 SCSI
chip. Commodore's ROM does the rest: scanning the bus, parsing the Rigid Disk Block, loading the
filesystem and booting. Two issues came up:
- Amiga Forever's boot disks set the RDB "last disk" flag, so the real driver stopped scanning after
  unit 0. The flag is cleared in memory on every disk but the last, as HDToolBox would do.
- Some images are bare partitions without an RDB. One is synthesized in front of them, describing
  the partition, as if a real drive had been partitioned.

### The NCR 53C710 runs its own programs
The A4091 and A4000T SCSI chip is a small processor. The driver writes SCRIPTS programs into
memory, and the chip fetches and runs them by DMA. They select targets, move data per bus phase,
branch on phases and message bytes, do register arithmetic, and copy memory, including into the
chip's own registers. The emulator implements that instruction set, plus the target's side of the
bus: IDENTIFY, synchronous transfer negotiation, MESSAGE REJECT, ATN, and the ACK held after a
message byte.

The details came from reverse-engineering the drivers with the built-in disassembler:
- The chip is wired big-endian, so a byte register sits at its data-manual offset XOR 3.
- The 53C710 supports relative jumps (bit 23), which I had assumed arrived later in the 53C720.
- Both drivers start their scripts through an alias of the register block: +$40000 on the A4091,
  +$80 on the A4000T. Writes to the documented address alone never start anything.

### PCMCIA cards: memory or disk?
An SRAM card in an A1200 can become extra Fast RAM or a CC0: disk. Disassembling card.resource
showed how Kickstart decides. It looks for a CISTPL_FORMAT tuple in the card's attribute memory.
If there is none, an SRAM or DRAM card is memory-tested and added to the free memory list. So a
DOS-formatted card gets a FORMAT tuple, as Commodore's card preparation tool would write, and
Kickstart mounts it as CC0: by itself. Any host folder can be passed as a card and is built into a
formatted FFS card image.

### Host folders as disks
Amiga Forever ships AROS and its OS 3.X installation as plain folders, which UAE mounts with its own
filesystem handler. RustyAmiga instead builds each folder into an FFS partition image when the
machine starts, so Kickstart's own FFS mounts it. Host names become Latin-1, long names are
shortened, and script files get the S protection bit.

### Demos that expect a JIT
Many packages were tuned on UAE's JIT, far faster than any real 68040, and looked blank because
they were still precalculating. Those packages get an adaptive CPU speed in the window: the
multiplier rises while the host keeps up and drops when it doesn't. Headless runs use a fixed
speed, so test results are repeatable.

### A virtual graphics card
Amiga Forever's enhanced OS 3.X expects UAE's graphics card, uaegfx: Picasso96 opens
`uaegfx.card` and puts Workbench on an 800x600 screen. That card has never existed as hardware.
Its driver is built by the emulator, and in modern WinUAE it no longer comes from disk: a ROM
creates `uaegfx.card` in memory at boot. RustyAmiga does the same. A resident in a small ROM at
`$F00000` builds the library, and InitCard points the BoardInfo functions at stubs that call the
emulator. The first version left drawing to Picasso96's own software routines. Workbench then
booted and sat idle on an empty grey screen, with only a few lines drawn. Amiga Forever's
rtg.library leaves those routines to the card, so the emulator now does the drawing too: rectangle
fills, copies with any minterm, text templates, patterns and planar-to-chunky conversion, straight
into video RAM. Booting got faster as well.

### Tools built along the way
- **Instruction tracing:** `TRACE_RANGE`, `BREAK_AT` and `DUMP_MEM` show what software does at a
  given point, with beam position and cycle count.
- **Disassembler:** `examples/disasm.rs` disassembles raw ROMs and drivers for reverse-engineering.
- **Scripted headless runs:** headless mode can type keys at a given frame, save screenshots at
  intervals, and record audio. A scratch folder with its own Startup-Sequence, mounted with
  `--dir-hd`, scripts tests inside AmigaOS.

## Where it is going

The plan in [docs/ADDONS.md](docs/ADDONS.md) is to offer every Amiga add-on board as a machine
option, by emulating each chip family once and describing boards as data on top of it. Next are more
storage controllers (53C9x, 5380, IDE boards), real RTG graphics cards, accelerators with the 68060 and
MMU, sound and network cards, and the CD32 FMV module.
