# Nokia 3310 Emulator

<img width="560" height="534" alt="image" src="https://github.com/user-attachments/assets/414cab30-18db-4594-bbb2-a963ab254e96" />


A hardware-level emulator of the **Nokia 3310** (DCT3 platform), written in Rust.
It does not reimplement the phone's UI — it emulates the chip and runs the
**original, unmodified Nokia firmware** on a virtual ARM7TDMI core, booting the
real phone software all the way.

> ⚠️ **No firmware is included.** The emulator ships with *no* ROM. You must supply
> your own firmware images, dumped from a phone you own (see [Firmware](#firmware)).

---

## Status

What currently works:

- **Boots official firmware v6.39** from the reset vector through the hardware
  self-test → boot animation → **PIN entry screen**, with working digit entry and
  PIN acceptance.
- **LCD** — PCD8544-style 84×48 monochrome framebuffer, rendered to a window.
- **Matrix keypad** — digits and navigation keys mapped to the PC keyboard.
- **SIM card** — ISO-7816 model (ATR + APDU), enough to drive SIM init and the
  PIN prompt. A test-SIM IMSI bypass is available for the SIM-lock gate.
- **Timers** calibrated to the real 13 MHz crystal (programmable timer ≈129 Hz,
  sleep clock 1057 Hz), so UI/animation speed and keypad auto-repeat behave
  correctly.
- **Audio** via [cpal](https://crates.io/crates/cpal):
  - Buzzer (PUP square-wave) for ringtones and warning/game tones.
  - DSP-generated key tones routed through the call-speaker path — dual-tone DTMF
    for digit keys, single tone for navigation keys.
- **Cycle-based timing** — a wait-state model (slow flash, fast RAM) drives the
  timers and paces execution, with the CPU kept at its native 13 MHz.

Known limitations:

- **No GSM baseband / network.** The DSP L1 layer is only partially stubbed, so the
  phone reaches the PIN/idle screens but cannot register to a network. Getting
  *past* the PIN to the live menu is gated by a network handshake that is not yet
  emulated.
- **Key-tone volume is fixed** — the keypad-tone level is applied in the analog
  COBBA codec (an undocumented serial ASIC we do not model); tones are synthesized
  directly from the DSP command at a constant amplitude.
- Charging, real RF, and the full DSP/baseband are not emulated.

---

## Hardware background

The Nokia 3310 (model NHM-5) is built around the Texas Instruments **MAD2WD1**
ASIC, which integrates:

- an **ARM7TDMI** CPU running at **13 MHz** (ARM + Thumb, no MMU/cache),
- a **TMS320-family DSP** for the GSM L1 / baseband (largely out of scope here),
- timers, an interrupt controller (IRQ/FIQ), serial I/O, and glue logic.

The firmware is **big-endian**. The display is a PCD8544-class controller
(84×48 px, organized as 6 banks of 8 px). Audio analog routing is handled by the
external **CCONT** (power/audio) and **COBBA** (codec) ASICs.

### Memory map

| Region                | Range                  | Notes                                   |
|-----------------------|------------------------|-----------------------------------------|
| RAM                   | `0x000000 – 0x200000`  | 2 MB                                    |
| DSP shared memory     | `0x010000 – …`         | MDI mailbox / L1 command queue          |
| MMIO peripherals      | `0x020000 – …`         | timers, keypad, LCD, CCONT, SIM, MBUS   |
| DSPIF / MCUIF         | `0x030000 / 0x040000`  | DSP ↔ MCU interface kicks               |
| Flash (firmware+PPM)  | `0x200000 – 0x3D0000`  | code + fonts/text/bitmaps/ringtones     |
| EEPROM / PM           | `0x3D0000 – 0x400000`  | persistent settings (in-memory copy)    |

The MAD2 peripheral register addresses are undocumented; they were recovered by
tracing (see [Methodology](#methodology)).

---

## Architecture

Cargo workspace:

```
crates/
├─ arm7tdmi/              ARM7TDMI CPU core (ARM+Thumb) behind a `Bus` trait,
│                         adapted from rustboyadvance-ng (MIT)
├─ rustboyadvance-utils/  support crate for the CPU core
├─ emucore/              the machine: memory map, peripherals, loader, tracer,
│  └─ src/                interactive debugger
│     ├─ machine.rs       bus routing, execution loop, IRQ/FIQ, flash FSM
│     ├─ lcd.rs           PCD8544 display
│     ├─ keypad.rs        matrix keypad
│     ├─ ccont.rs         CCONT power/audio controller (ADC, battery, RSSI)
│     ├─ ctsi.rs          timers + interrupt controller
│     ├─ sim.rs           ISO-7816 SIM card (ATR/APDU)
│     ├─ mbus.rs          MBUS/FBUS serial
│     ├─ dsp.rs           DSP MDI transport / L1 stubs
│     ├─ buzzer.rs        PUP buzzer audio model
│     ├─ flash.rs         NOR flash command FSM
│     ├─ loader.rs        assembles the flash image from .fls dumps
│     └─ bin/dbg.rs       interactive debugger REPL
└─ nokia3310/            frontend binary: window (minifb), keyboard, cpal audio
```

The CPU core is decoupled from the rest via a `Bus`/`MemoryInterface` trait, so
the GBA-era timing of the original core is replaced by the DCT3 wait-state model.

---

## Firmware

The firmware is copyrighted by Nokia and is **not** distributed with this project.
To run the emulator, place your own dumps in `crates/rom/` (they are git-ignored):

- `3310 v6.39 Converted MCU+PPM B.fls` — MCU + PPM image (mapped at `0x200000`).
  The loader should report `firmware id : 39 23-12-04 NHM-5`.
- `3310-607 EEPROM.fls` — EEPROM image (mapped at `0x3D0000`).

The `.fls` files are raw region dumps (no container header); the loader recognizes
them by size and base address. Keep only the v6.39 image in `crates/rom/` — two
files of the same size make the loader's choice non-deterministic.

The `.fls` files are read into an in-memory copy; the emulator never writes back to
the dumps on disk.

---

## Build & run

Requires a stable Rust toolchain.

```sh
# With firmware in place under crates/rom/:
./RUN_3310.sh
```

`RUN_3310.sh` launches the frontend with the exact environment combination that has
been verified to boot v6.39 to the PIN screen. Each variable matters:

| Variable             | Purpose                                                            |
|----------------------|-------------------------------------------------------------------|
| `DSP_FIQ_AT=20000`   | DSP model signals readiness so the self-test passes               |
| `TIMER_AUTORELOAD=1` | periodic programmable timer (the OS tick v6.39 depends on)         |
| `SELFTEST_SUB=1`     | subscribes to self-test results (pub/sub) so it actually completes |
| `REG_ALL=1`          | indication-deliverability bitmap → MMI events flow                 |
| `ST_PASS=1`          | clean self-test pass → no watchdog reboot / "CONTACT SERVICE"      |
| `SIM_ATR=1`          | enables the SIM ATR/APDU model → SIM init → PIN prompt             |

To get past the SIM-lock gate after the PIN, add `SIM_IMSI=001011234567890` (a test
MCC=001 SIM that bypasses the lock).

Or run it directly:

```sh
DSP_FIQ_AT=20000 TIMER_AUTORELOAD=1 SELFTEST_SUB=1 REG_ALL=1 ST_PASS=1 SIM_ATR=1 \
  cargo run --release -p nokia3310
```

### Controls

| PC key                  | Phone key            |
|-------------------------|----------------------|
| Arrow Up / Down         | Scroll up / down     |
| Enter                   | Menu / Select        |
| Backspace (or C)        | C / Clear / Back     |
| `0`–`9` / Numpad        | Digits `0`–`9`       |
| `[`                     | `*`                  |
| `]`                     | `#`                  |
| `P`                     | Power (auto-held ~2 s at startup) |
| `Z` / `X`               | decrease / increase flash wait-states (timing debug) |
| `Esc`                   | quit                 |

---

## Methodology

Because the MAD2 register map is closed, the core technique is **bring-up by
tracing**: run the CPU, log every access to unknown addresses together with the
program counter, deduce what each peripheral does, implement the minimum to get
further, and repeat. Open-source DCT3 references are used as cheat-sheets:

- **MADos** — open DCT3 firmware; its source reveals which registers are touched
  and in what order.
- **NokiX** — firmware modding / register documentation.
- **MAME** `nokia_3310.cpp` — MAD2 register names.
- **Project Blacksphere**, **swsim** — hardware/SIM reverse-engineering.

An interactive debugger (`cargo run -p emucore --bin dbg`) provides `run_until`,
memory/register read-write, watchpoints, and a live screen dump for diagnosing
boot behavior without rebuilding.

---

## Credits & licensing

- Marcin Selerowski + Claude Code (mostly Opus 4.8)
- The ARM7TDMI core is adapted from
  [**rustboyadvance-ng**](https://github.com/michelhe/rustboyadvance-ng) (MIT).
- Reverse-engineering leans on the **MADos**, **NokiX**, **swsim**, and **MAME**
  projects (see above).

This emulator contains **no Nokia firmware or other copyrighted ROM data**. You are
responsible for supplying firmware you are legally entitled to use.
