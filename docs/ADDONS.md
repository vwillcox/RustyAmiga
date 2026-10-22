# Add-on boards: survey and emulation plan

The goal is to offer every Amiga add-on board as a machine option. There were well over a
thousand products, but they fall into a few dozen families. Within a family the boards share
chips and differ in IDs, address maps, ROMs and wiring. So the plan is to emulate the chips once,
describe each board as data on top of them, and let a machine take any combination that would have
physically fitted it.

The board list draws on WinUAE's expansion tables, which name about 124 expansion boards,
40 graphics cards, 35 accelerators and 12 special display devices with known register maps and
ROM dumps, and on the Amiga hardware databases (amiga.resource.cx, Big Book of Amiga Hardware).

## 1. Where boards plug in

| Machine | Expansion connectors |
|---|---|
| A1000 | 86-pin side slot (Zorro I-style: sidecars, SCSI/RAM boxes); front 256 KB RAM slot (A1050); WOM daughterboard |
| A500 / A500+ | 86-pin side slot (autoconfig; A590, A570 CD-ROM, Supra/GVP HD8+/Impact, Action Replay); trapdoor (A501 512 KB + RTC, 1 MB A500+ boards); CPU socket (68000 adapters: accelerators such as GVP A530, Supra Turbo 28, Mega Midget Racer) |
| A600 | PCMCIA Type II (SRAM/flash, CF, network, CD-ROM adaptors, Overdrive HD); trapdoor (A601: 1 MB chip + RTC); CPU on board: clip-on accelerators (A600 TF/Furia) |
| A1200 | PCMCIA Type II; 150-pin trapdoor CPU slot (Blizzard, Apollo, GVP A1230, Phase 5 PPC, ACA, TF12xx); clock port (Silver Surfer, Prelude 1200, Delfina Lite, Subway USB, RapidRoad); internal IDE (2.5"; 3.5" and ATAPI via adaptors, 4-way IDE splitters such as Buddha-style) |
| A2000 (A/B/C) | 5 × Zorro II; 86-pin CPU slot (A2620/A2630, GVP, CSA, Warp/Commodore 68030/40); video slot (A2320 flicker fixer, A2300 genlock, Video Toaster, Picasso II-style passthrough); 4 × ISA (2 in line with Zorro for Bridgeboards A2088/A2286/A2386SX) |
| A3000 / A3000T | 4 × Zorro III (T: 5); 200-pin CPU slot (A3640, Fusion Forty, Warp Engine 3040, CyberStorm, Phase 5 PPC); video slot; built-in SDMAC SCSI ✓ |
| A4000 / A4000T | 5 × Zorro III (daughterboard); CPU slot (A3640/A3660, CyberStorm MK I-III/PPC, Warp Engine, GVP G-Force 040/060, Apollo 4040/4060); video slot; IDE ✓ (T: NCR 53C710 SCSI too) |
| CDTV | Expansion slot (Zorro II-style, 1 card: A2091-style SCSI, RAM); video slot (genlock); front memory card (CDTV SRAM/bookmark cards); A570 is a CDTV for the A500 |
| CD32 | Expansion port (FMV module: MPEG video/audio; SX-1/SX32 Pro: RAM, IDE, PC keyboard, parallel/serial, floppy, accelerator) |
| All | Serial port, parallel port, joystick/mouse ports, floppy port (external drives), RGB/composite video out (genlocks, digitizers, flicker fixers, HAM-E/DCTV decoders) |

## 2. What is emulated today

- The autoconfig chain and board registry (step 1 below): generic and named Zorro II/III RAM boards,
  the A2091 with optional RAM, trapdoor boards (A501, A601, MBX 1200z), and the battery-backed
  clocks (MSM6242B, RF5C01A). Motherboard RAM (A3000/A4000) and slow RAM.
- A590/A2091 SCSI (DMAC + WD33C93, Commodore boot ROM), A3000 SDMAC SCSI, Gayle IDE (A600/A1200), A4000 IDE.
- NCR 53C710 with its SCRIPTS processor: the A4091 (Commodore boot ROM) and the A4000T's built-in SCSI.
- SCSI CD-ROM drives with ISO images, on every SCSI controller.
- CD32 Akiko (ID, C2P, NVRAM) without a CD drive.
- Host folders as FFS volumes (instead of UAE's host filesystem board).
- CPUs 68000/68020/68030/68040 (no MMU translation, no 68060), FPU 68881/68882/68040.

Missing basics that many boards build on: the 68060, and MMU translation (needed by some accelerator
ROMs and by software such as Enforcer and VMM).

## 3. Families and how to emulate them

Each row is one piece of emulation (a chip core or a mechanism). The boards listed then need only
a description: bus, autoconfig IDs, register offsets, ROM, interrupt line and quirks.

### Memory
| Family | Boards (examples) | Emulation |
|---|---|---|
| Autoconfig fast RAM (Zorro II/III, side slot) | A2052, A2058, A590/A2091 RAM, GVP Series II RAM, Supra 500RX, MicroBotics 8-Up, BSC MemoryMaster, Zorro III: A3000/A4000 cards, BigRAM, ZZ9000 RAM | Generic RAM board parameterized by manufacturer/product ID, size, bus (Z2/Z3). One table entry per product |
| Trapdoor/slow RAM | A501, A601, A1050, A500+ 1 MB, Supra trapdoor, A1200 trapdoor RAM (on accelerators) | Slow RAM at $C00000 or chip RAM, plus the RTC where the board has one |
| Chip RAM upgrades | Fat Agnus 8372A, MegAChip 2000/500 (2 MB chip) | Agnus revision and chip RAM size options |
| CDTV/PCMCIA memory cards | CDTV SRAM bookmark card, A600/A1200 SRAM/flash cards | Card image file, CardResource-compatible attribute memory (CIS) |

### Accelerators (CPU slot, trapdoor, CPU socket)
| Family | Boards (examples) | Emulation |
|---|---|---|
| 68020/68030 | A2620, A2630, GVP A3001/A530/Combo, Blizzard 1230 I-IV, Apollo 630/1230, M-Tec E-Matrix 530, DKB 1230/Cobra, CSA Twelve Gauge, Mega Midget Racer, TF530/TF1230 | CPU model + clock multiplier + on-board fast RAM map + board ROM (some remap Kickstart to fast RAM: MapROM) + optional SCSI (see below) |
| 68040/68060 | A3640, A3660, Fusion Forty, Warp Engine, G-Force 040/060, CyberStorm MK I/II/III, Blizzard 1260/2060, Apollo 1240/1260/4060, Zeus 040, Magnum 40, TQM, TF1260 | As above, plus a 68060 core (FPU/integer subset traps for 68060.library), and 040/060 MMU for the ROMs that set it up |
| PowerPC dual-CPU | CyberStorm PPC, Blizzard PPC (603e/604e + 68040/060) | A PowerPC core alongside the 68k with the boards' shared-memory protocol (WarpOS/PowerUP). WinUAE uses a QEMU PPC plugin. AF ships the CyberStorm PPC ROM |
| Modern FPGA/Pi | Vampire (68080/SAGA), PiStorm, Warp 1260, ACA series | ACA/Warp behave like the 68030/060 boards above. Vampire's 68080 and SAGA chipset are a separate project. PiStorm is an emulator itself and is out of scope |

### Storage controllers
| Chip family | Boards (examples) | Emulation |
|---|---|---|
| WD33C93 SCSI ✓ | A2091/A590 ✓, A3000 ✓, A2090/A2090A, CDTV SCSI, GVP Series I/II, GVP A1208, Comspec SA | Core done. Each board: DMA engine + ROM |
| NCR 53C710/770 SCSI ✓ | A4091 ✓, A4000T ✓, Warp Engine, CyberStorm MK III/PPC, Blizzard PPC, Tek Magic 2040/2060, Zeus 040, Magnum 40, QuikPak, Wildfire, DraCo | Core done (53C710). The 53C770 adds registers and SCRIPTS features. Each board: address map + ROM |
| NCR 53C9x / FAS216/236 (ESP) | Fastlane, Oktagon 2008, Blizzard SCSI Kit III/IV, CyberStorm MK I/II SCSI, Masoboshi, Golem Fast, Trifecta, DKB RapidFire/SpitFire, M-Tec Mastercard/E-Matrix, Multi Evolution, SCRAM (53C94), ALF3, Surf Squirrel (PCMCIA), Typhoon MK2 | New core (command-driven, simpler than 53C710) |
| NCR 5380 and other simple controllers | Supra, Trumpcard/Grand Slam, Nexus, HardFrame, StarDrive, FastTrak, Kommos, Malibu, Kronos, Golem SCSI II, Evesham Reference, AdSCSI, Apollo, DataFlyer, Emplant, IVS Vector, Twelve Gauge, Fireball, InMate, Phoenix, AddHard, Overdrive, ALF1/2, SCRAM (DP8490), Paradox (parallel port) | New core (very simple, CPU-driven); about 40 boards on top of it |
| Early SASI/MFM | Xebec 9720H, Tecmar, OMTI, Combitec HD20A | Small custom cores over the SASI protocol |
| IDE/ATA ✓ | Gayle ✓, A4000 ✓, Buddha/Catweasel IDE, AlfaPower/AT-Bus 2008, Elbox FastATA, AccessX, Arriba, AdIDE, Tandem, A-Team, RIPPLE, AIDE, Trumpcard 500AT, Golem IDE, Dotto, X-Surf IDE | Core done; each board is a register-map description + ROM |
| ATAPI CD-ROM, SCSI CD-ROM ✓ (SCSI) | CD-ROM drives on any of the above, A570/CDTV drive | ATAPI/SCSI MMC packet commands over an ISO/CUE image; shared with the CD32 drive work |
| SCSI tape | A3070 (QIC-150), DAT drives | SCSI sequential-access device backed by a file; with the drive noises, a working tape drive |
| Floppy | A1010/A1011/A1020 (5.25" 40-track), A2010, HD drives ✓, Catweasel (PC/C64/Mac formats) | Drive geometry options; Catweasel as its own controller with raw disk images |

### Graphics (RTG) and display hardware
| Family | Boards (examples) | Emulation |
|---|---|---|
| UAE virtual RTG (uaegfx) | Amiga Forever's own setup | UAE's trap interface: lets the AF System folder run in high colour. High value, moderate work |
| Cirrus GD542x/543x/5446 | Picasso II/II+, Picasso IV (ROM in AF), Piccolo/SD64, Spectrum 28/24, GD5446 PCI | VGA core + Cirrus BitBLT; driven by Picasso96/CyberGraphX drivers in AmigaOS |
| S3 Trio64/ViRGE | CyberVision 64, CyberVision 64/3D, S3 PCI cards | S3 core (+ ViRGE 3D for 64/3D) |
| Tseng ET4000 | Merlin, Domino, oMniBus, Visiona | ET4000 core |
| Others | Retina (NCR 77C32BLT), A2410/Rainbow III (TI TMS34010/20 GSP), Permedia2 (CyberVision PPC), Voodoo 3/5, Matrox, Harlequin, EGS 110/24, Graffity, Pixel64, Altais | One core each; TMS340x0 means emulating a second CPU |
| Display passthrough decoders | HAM-E, DCTV, A2024, Firecracker 24, Graffiti, Video DAC 18, ColorBurst, AVideo 12/24, OpalVision | Decode special pixel data from the normal chipset output, as WinUAE's special monitors do. Cheap and fun |
| Flicker fixers / scan doublers | A2320, Indivision ECS/AGA, the A3000's built-in display enhancer | Display options: deinterlace and line doubling in the frontend |
| Genlocks, Video Toaster, digitizers | A2300, GVP G-Lock, Video Toaster, DigiView, VLab, FrameMachine | Genlock: overlay host video/image behind colour 0. Digitizers: feed a host camera or image file. Video Toaster: needs its own processors (a long-term item) |

### Sound, MIDI and sampling
| Family | Boards (examples) | Emulation |
|---|---|---|
| Codec cards | Toccata (AD1848), Prelude/Prelude 1200 (AD1816A), ES1370/FM801 on PCI bridges | Codec + FIFO + interrupts; AHI drivers in AmigaOS. Host audio output and input |
| DSP cards | Delfina (DSP56002), Melody (MPEG decoder), Maestro Pro | DSP core needed (Delfina); Melody decodes MPEG audio |
| Parallel-port samplers | Perfect Sound, AMAS, Megalosound, Aura (PCMCIA), Stereo Master | Read host microphone into the parallel port data lines |
| MIDI interfaces | Any serial-port MIDI interface | Serial port bridged to host MIDI |

### Communications and I/O
| Family | Boards (examples) | Emulation |
|---|---|---|
| Ethernet | A2065 (Am7990), Ariadne (Am79C960), Ariadne II/X-Surf (RTL8019/8029), Hydra/LAN Rover (DP8390), X-Surf 100 (AX88796), PCMCIA NE2000 cards (Netgear FA411), A2060 ARCnet | One core per chip, with a host backend: user-mode NAT (works without privileges) or a TAP device |
| Multi-serial / parallel | A2232 (65CE02 + 7 × 6551 ACIA), GVP IO Extender, Multiface (16C550s), Silver Surfer (clock port 16C550), ioblix | UART cores; host backends: PTY, TCP, file. A2232 needs a 65CE02 CPU |
| USB | Subway, Highway, Deneb, RapidRoad | ISP1160/SL811 USB host controller + host USB passthrough; long-term |
| Real-time clock | A501/A601 RTC, A3000/A4000 RF5C01A, OKI MSM6242B boards | Small chip cores tied to the host clock; needed for most setups |
| Printer | Any parallel printer (AF RP9s name an Epson printer) | Capture parallel output to a file; optionally render Epson ESC/P to PDF |
| Input devices | CD32 pad, light guns/pens (Trojan Phazer), tablets, trackballs, joysticks, keypads | Port-level emulation; light pens latch the beam position from the host mouse |
| PC Bridgeboards | A1060 Sidecar, A2088(T), A2286, A2386SX | A full x86 PC (WinUAE embeds DOSBox's core) talking through shared memory; long-term |
| Freezer cartridges | Action Replay I-III, Nordic Power, HRTMon | Cartridge ROM + NMI button + memory mirror; ROM needed |
| Kickstart and ROM boards | Multi-Kickstart boards, A1000 WOM, AMAX dongle, custom ROM boards | ROM mapping options |

## 4. How a board becomes an option

1. **Expansion framework.** Each board is an object that knows its bus and slot, its autoconfig
   identity (Zorro II/III, or none), its address ranges once configured, its interrupt line,
   and its per-line tick. The machine builds an autoconfig chain from the boards in slot order,
   replacing today's hard-coded fast RAM / A2091 / Zorro III RAM code.
2. **Board registry.** A table of every known board: id, display name, manufacturer, year, machines
   it fits, slot type, the chip families it uses, the ROMs it needs (file names and checksums),
   and user options (RAM size, jumpers, host backend).
3. **Machine configuration.** A list of boards on the command line (`--board a2091`,
   `--board picasso4:z3`, `--board blizzard1260:ram=64`) and in a launcher "Expansion" page that
   only offers boards that fit the selected model and shows which ROMs are missing. RP9 manifests
   map onto boards where they name hardware (rtg, jit, cpu boards, PPC).
4. **ROM handling.** Boards that need a ROM look it up like Kickstarts do (Amiga Forever ships the
   A590, A4091, Picasso IV, CyberStorm PPC, CD32 FMV and CDTV ROMs); others name the dump they need.
5. **Testing.** Each board gets an automated boot test with its own driver or ROM: it autoconfigures,
   its driver finds it, and a disk on it mounts or a screen opens on it.

## 5. Order of work

Ordered by what unblocks the most boards and by usefulness:

1. **Framework and basics:** expansion bus/registry refactor; the existing boards and RAM boards moved
   onto it; the RTC; a table of RAM boards; the MapROM and memory-map side of accelerators.
2. **Storage:** NCR 53C9x, NCR 53C710 (A4091 with the AF ROM, A4000T), NCR 5380, then the IDE board
   variants; CD-ROM (ATAPI/SCSI) and SCSI tape on all controllers; PCMCIA storage.
3. **Graphics:** uaegfx (for the Amiga Forever System folder), then Cirrus (Picasso II/IV) and S3
   (CyberVision 64), the display decoders (HAM-E, DCTV, A2024, Firecracker) and flicker-fixer options.
4. **Accelerators:** the 68060, MMU translation, then the board catalog (Commodore, GVP, Phase 5,
   Apollo, MacroSystem, DKB, CSA, ACA, TF).
5. **Sound and I/O:** Toccata/Prelude with AHI, serial cards (Multiface, Silver Surfer), MIDI, samplers,
   printer capture, ethernet (A2065, Ariadne, X-Surf) with user-mode networking.
6. **CD-based machines:** CD32 CD drive and FMV module, CDTV, A570.
7. **Large items:** PowerPC boards, x86 Bridgeboards, A2232, TMS340x0 cards, Delfina, Video Toaster,
   USB controllers.

Boards with no surviving documentation or ROM dump can be listed in the registry as known but
unavailable, so the catalog stays complete even where emulation is not possible.
