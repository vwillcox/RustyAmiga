# RustyAmiga

A Commodore Amiga emulator written in Rust, from the A1000 to the A4000T and the CD32.

RustyAmiga emulates the hardware itself and runs the genuine Kickstart ROMs and Commodore's own
device drivers. It doesn't patch or replace them, so disks, hard drives and expansion boards
behave as they did on a real machine.

## What it runs

- **Machines:** A1000, A500, A500+, A600, A2000, A1200, A3000, A4000, A4000T, the CD32 (with
  its CD drive, CD audio, joypad and the FMV cartridge for Video CDs) and the CDTV (with its CD
  drive, player and remote). Their NVRAM is kept between sessions in `~/.config/amiga/nvram`.
- **Chipsets:** OCS, ECS and AGA, including HAM8, the AGA sprite and fetch modes, and the
  undocumented tricks that demos rely on.
- **CPUs:** a cycle-exact 68000 (it matches all 261,894 SingleStepTests cases, including cycle
  counts), plus the 68020, 68030 and 68040, and the 68881, 68882 and 68040 FPUs.
- **Storage:** floppies (DD and HD), A590/A2091 and A3000 SCSI, A600/A1200/A4000 IDE, the A4091 and
  A4000T NCR 53C710 SCSI, SCSI and ATAPI CD-ROM drives (ISO or CUE/BIN), PCMCIA memory cards,
  and host folders mounted as hard disks.
- **Expansion:** Zorro II/III autoconfig, RAM boards with their real IDs, trapdoor boards,
  battery-backed clocks, and UAE's virtual graphics card (uaegfx) for Picasso96 screens up to
  1920x1080.
- **Amiga Forever titles:** most of the games and demos in the Amiga Forever collection run
  straight from their `.rp9` packages.
- **Extras:** a graphical launcher, synthesized floppy and hard disk drive noises, front-panel
  lights, and gamepads. The launcher's Controls tab maps keys and gamepad buttons to the
  joysticks and the CD32 pad, and can put a second joystick in port 1.

## Building

```sh
cargo build --release
./target/release/amiga            # opens the launcher
./target/release/amiga game.rp9   # runs a title directly
./target/release/amiga --help     # every option
```

On Linux, `tools/install-desktop.sh` adds RustyAmiga to the desktop's application menu.

## ROMs and media

The Amiga's custom chips (Agnus, Denise, Paula and the rest) contain no ROM code, so nothing is
needed for them. What an Amiga does need is a **Kickstart ROM**, the boot firmware and core of
AmigaOS. Some expansion boards also need their own **boot ROM**. These are still under copyright, so
none are included. Here are the legal ways to get them.

### Kickstart and board ROMs

- **[Amiga Forever](https://www.amigaforever.com/)** by Cloanto, which holds the licence to the
  classic Amiga ROMs and OS. This is the easiest option, and the one RustyAmiga is built around. The
  Plus and Premium editions include every Kickstart version (1.0 to 3.X, for each model). They also
  include board ROMs that RustyAmiga uses: the A590/A2091 and A4091 SCSI boot ROMs, the CD32
  extended ROM, the CD32 FMV cartridge ROM (in Amiga Forever 11), and the CDTV and A570 extended
  ROMs. On top of that, they include the Workbench
  disks and a collection of licensed games and demos as `.rp9` packages. The ROMs are encrypted;
  copy them together with their `rom.key` and RustyAmiga decrypts them as it loads them. Check
  Cloanto's edition comparison before buying, because the cheaper editions include fewer ROMs.
- **Your own Amiga.** If you own the hardware, you can copy its ROM to a PC with Amiga Explorer
  (included with Amiga Forever) over a cable. A ROM-dump tool such as TransROM, or an EPROM reader,
  also works. The legality of making
  a personal copy varies by country.
- **AmigaOS 3.1.4 / 3.2** from [Hyperion Entertainment](https://www.hyperion-entertainment.com/) are
  modern updates of the classic OS, sold for real Amigas through Amiga dealers. Check the package
  contents for whether ROM files for emulators are included.
- **[AROS](https://aros.sourceforge.io/)** is a free, open-source replacement for Kickstart and
  AmigaOS (AROS Public License). It needs no purchase, and RustyAmiga boots its ROM. It runs
  system-friendly software well, but games and demos that rely on the original Kickstart's internals
  may not work.

ROMs dumped from real hardware and unencrypted ROM files also work; pass them with `--rom`.

### Software

- **Amiga Forever** includes many licensed games and demos ready to run.
- **Demos** are generally free to download from their authors and from archives such as
  [scene.org](https://www.scene.org/).
- **Games:** many publishers and authors have released their Amiga games for free download; look
  for an explicit release statement, and buy the rest from their rights holders where they are
  still sold. Copies of commercial games found on general download sites are usually not legal.

### Where RustyAmiga looks for files

| Folder | Contents |
|---|---|
| `roms/` | Kickstart and board ROMs, and `rom.key` (Amiga Forever file names, such as `amiga-os-310-a1200.rom`) |
| `hdf/`, `adf/` | Amiga Forever's shared boot disks (`Amiga Files/Shared/hdf` and `adf`) |
| `dir/` | Amiga Forever's shared system folders: AROS and OS 3.X (`Amiga Files/Shared/dir`) |
| `titles/` | `.rp9` packages |

## Examples

```sh
amiga --model a1200 disk.adf
amiga --model a4000 --board a4091 --hdf workbench.hdf --cd disc.iso
amiga --model a600 --pcmcia some-folder/      # the folder becomes a formatted memory card
amiga --model cd32 --board cd32fmv --cd movie.cue   # plays a Video CD
amiga --list-boards
```

## More

- [ABOUT.md](ABOUT.md): how it works, and the problems solved along the way.
- [docs/STATUS.md](docs/STATUS.md): detailed status and known issues.
- [docs/ADDONS.md](docs/ADDONS.md): the plan for emulating every Amiga add-on board.

## Status

RustyAmiga is a work in progress. Not yet emulated: real RTG graphics cards, MMU address
translation, the 68060, and PowerPC boards.

## Licence

RustyAmiga is licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. Unless you explicitly state otherwise, any contribution you submit for inclusion in
this project, as defined in the Apache-2.0 license, is dual licensed as above, without any additional
terms or conditions.

The licence covers RustyAmiga's own code only. Kickstart ROMs, AmigaOS and Amiga software are
copyrighted by their owners and are not part of this project. Amiga is a trademark of its owners;
RustyAmiga is not affiliated with them.
