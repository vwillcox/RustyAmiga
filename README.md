# RustyAmiga

A Commodore Amiga emulator written in Rust, from the A1000 to the A4000T and the CD32.

RustyAmiga emulates the hardware itself and runs the genuine Kickstart ROMs and Commodore's own
device drivers. It doesn't patch or replace them, so disks, hard drives and expansion boards
behave as they did on a real machine.

## What it runs

- **Machines:** A1000, A500, A500+, A600, A2000, A1200, A3000, A4000, A4000T, and the CD32
  (which doesn't have a CD drive yet).
- **Chipsets:** OCS, ECS and AGA, including HAM8, the AGA sprite and fetch modes, and the
  undocumented tricks that demos rely on.
- **CPUs:** a cycle-exact 68000 (it matches all 261,894 SingleStepTests cases, including cycle
  counts), plus the 68020, 68030 and 68040, and the 68881, 68882 and 68040 FPUs.
- **Storage:** floppies (DD and HD), A590/A2091 and A3000 SCSI, A600/A1200/A4000 IDE, the A4091 and
  A4000T NCR 53C710 SCSI, SCSI CD-ROM drives, PCMCIA memory cards, and host folders mounted as
  hard disks.
- **Expansion:** Zorro II/III autoconfig, RAM boards with their real IDs, trapdoor boards, and
  battery-backed clocks.
- **Amiga Forever titles:** most of the games and demos in the Amiga Forever collection run
  straight from their `.rp9` packages.
- **Extras:** a graphical launcher, synthesized floppy and hard disk drive noises, front-panel
  lights, and gamepad support.

## Building

```sh
cargo build --release
./target/release/amiga            # opens the launcher
./target/release/amiga game.rp9   # runs a title directly
./target/release/amiga --help     # every option
```

On Linux, `tools/install-desktop.sh` adds RustyAmiga to the desktop's application menu.

## ROMs and media

No ROMs or Amiga software are included. RustyAmiga uses the files from
[Amiga Forever](https://www.amigaforever.com/), including its encrypted ROMs and `rom.key`:

| Folder | Contents |
|---|---|
| `roms/` | Kickstart and board ROMs, and `rom.key` |
| `hdf/`, `adf/` | Amiga Forever's shared boot disks |
| `dir/` | Amiga Forever's shared system folders (AROS, OS 3.X) |
| `titles/` | `.rp9` packages |

## Examples

```sh
amiga --model a1200 disk.adf
amiga --model a4000 --board a4091 --hdf workbench.hdf --cd disc.iso
amiga --model a600 --pcmcia some-folder/      # the folder becomes a formatted memory card
amiga --list-boards
```

## More

- [ABOUT.md](ABOUT.md): how it works, and the problems solved along the way.
- [docs/STATUS.md](docs/STATUS.md): detailed status and known issues.
- [docs/ADDONS.md](docs/ADDONS.md): the plan for emulating every Amiga add-on board.

## Status

RustyAmiga is a work in progress. Not yet emulated: the CD32 and CDTV CD drives, RTG graphics
cards, MMU address translation, the 68060, and PowerPC boards.
