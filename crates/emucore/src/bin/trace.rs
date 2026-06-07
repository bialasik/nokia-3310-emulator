//! Harness bring-upu (M2): laduje ROM, odpala rdzen ARM7TDMI od ENTRY i single-stepuje,
//! pokazujac dezasemblacje pierwszych instrukcji oraz log dostepow do MMIO.
//!
//! Uzycie:  cargo run -p emucore --bin trace -- [ENTRY_HEX] [STEPS] [ASM_COUNT]
//! Domyslnie ENTRY = 0x00200040 (entry firmware za naglowkiem), STEPS=2000, ASM_COUNT=60.

use arm7tdmi::arm::ArmInstruction;
use arm7tdmi::thumb::ThumbInstruction;
use arm7tdmi::disass::Disassembler;
use arm7tdmi::memory::DebugRead;
use arm7tdmi::Arm7tdmiCore;
use emucore::machine::{FW_ENTRY, MMIO_END, MMIO_START, ROM_END, ROM_START};
use emucore::{loader, Machine};
use rustboyadvance_utils::Shared;

fn parse_hex(s: &str) -> Option<u32> {
    u32::from_str_radix(s.trim_start_matches("0x"), 16).ok()
}

fn disasm_thumb(pc: u32, half: u16) -> String {
    std::panic::catch_unwind(|| {
        let bytes = half.to_le_bytes();
        let mut d = Disassembler::<ThumbInstruction>::new(pc, &bytes);
        d.next().map(|(_, line)| line)
    })
    .ok()
    .flatten()
    .unwrap_or_else(|| format!("{pc:8x}:\t{half:04x} \t(?)"))
}

fn disasm(pc: u32, raw: u32) -> String {
    // Dezasembler oczekuje bajtow little-endian prawdziwego slowa.
    // Moze panikowac na nieprawidlowym kodzie warunku - lapiemy panike.
    std::panic::catch_unwind(|| {
        let bytes = raw.to_le_bytes();
        let mut d = Disassembler::<ArmInstruction>::new(pc, &bytes);
        d.next().map(|(_, line)| line)
    })
    .ok()
    .flatten()
    .unwrap_or_else(|| format!("{pc:8x}:\t{raw:08x} \t(dane/niedekodowalne)"))
}

fn main() {
    let rom = match loader::load() {
        Ok(Some(r)) => r,
        Ok(None) => {
            eprintln!("brak plikow ROM w roms/ ani crates/rom/");
            return;
        }
        Err(e) => {
            eprintln!("blad ladowania ROM: {e}");
            return;
        }
    };
    loader::print_map(&rom);

    let mut args = std::env::args().skip(1);
    let entry = args.next().and_then(|s| parse_hex(&s)).unwrap_or(FW_ENTRY);
    let steps: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(2000);
    let asm_count: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(60);
    // Od ktorego kroku zaczac drukowac dezasemblacje (ASM_FROM).
    let asm_from: u64 = std::env::var("ASM_FROM")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let watch = args.next().and_then(|s| parse_hex(&s)).unwrap_or(0x0001_00FE);
    // Co ile krokow asertowac IRQ (tick timera). 0 = wylaczone.
    // INT_OFF=1 wylacza asercje przerwan (timer counter dziala dalej).
    let irq_period: u64 = if std::env::var("INT_OFF").is_ok() {
        u64::MAX
    } else {
        0
    };

    let mut machine = Machine::new(rom.mem);
    machine.watch = Some(watch);
    // ROM_PATCH=addr:hex[,addr:hex...] - patch bajtow w kopii ROM w pamieci (plik nietkniety).
    if let Ok(spec) = std::env::var("ROM_PATCH") {
        for part in spec.split(',') {
            let mut it = part.splitn(2, ':');
            if let (Some(a), Some(v)) = (it.next().and_then(parse_hex), it.next().and_then(parse_hex)) {
                machine.patch_rom(a, v as u8);
                println!("[ROM_PATCH] {a:#08X} <- {:#04X}", v as u8);
            }
        }
    }
    let bus = Shared::new(machine);
    let mut cpu = Arm7tdmiCore::new(bus);

    cpu.reset();
    cpu.set_reg(15, entry);
    cpu.reload_pipeline32();

    // Statyczna dezasemblacja regionu bez wykonywania: DISM=addr:count[:t|a]
    // (t=thumb domyslnie, a=arm). Diagnostyka petli/poll bez czekania na N krokow.
    if let Ok(spec) = std::env::var("DISM") {
        let parts: Vec<&str> = spec.split(':').collect();
        let base = parts.first().and_then(|s| parse_hex(s)).unwrap_or(entry);
        let cnt: u32 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(40);
        let arm = parts.get(2).map(|s| *s == "a").unwrap_or(false);
        println!("\n--- DISM @{base:#010X} ({} instr, {}) ---", cnt, if arm {"ARM"} else {"THUMB"});
        let step = if arm { 4 } else { 2 };
        for k in 0..cnt {
            let a = base + k * step;
            if arm {
                let raw = u32::from_be_bytes([
                    cpu.bus.debug_read_8(a), cpu.bus.debug_read_8(a + 1),
                    cpu.bus.debug_read_8(a + 2), cpu.bus.debug_read_8(a + 3),
                ]);
                println!("  A {}", disasm(a, raw));
            } else {
                let half = u16::from_be_bytes([cpu.bus.debug_read_8(a), cpu.bus.debug_read_8(a + 1)]);
                println!("  T {}", disasm_thumb(a, half));
            }
        }
        return;
    }

    println!("\n=== TRACE: ENTRY={entry:#010X}, krokow={steps} ===\n");
    println!("--- Dezasemblacja wykonania (pierwsze {asm_count} instrukcji) ---");

    let mut escaped_at: Option<(u64, u32)> = None;
    let mut recent = [0u32; 24];
    let mut ridx = 0usize;
    let mut dumped = false;
    let mut clear_step: Option<u64> = None;
    let dump_pc = std::env::var("DUMP_PC").ok().and_then(|s| parse_hex(&s));
    let mut dump_cnt = 0;
    let mut peak_lit = 0usize;
    let mut peak_ascii = String::new();
    let mut peak_step = 0u64;
    let mut last_text_ascii = String::new();
    let mut last_text_lit = 0usize;
    let mut last_text_step = 0u64;
    // Trace wywolan funkcji (BL): licznik wg adresu docelowego.
    let mut call_count: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
    let trace_calls = std::env::var("CALLS").is_ok();
    let mut fiq_log_cnt = 0u32;
    let mut bank_corrupt_seen = false;
    let mut bank_was_set = false;
    let mut last_fiq_bank = 0u32;
    let mut bank_chg_cnt = 0u32;
    let mut reset_log_cnt = 0u32;
    let mut lowjump_seen = false;
    let mut watch_prev = 0usize; // do detekcji trafienia watch -> zrzut stosu
    let mut wwatch_prev = 0usize; // jw. dla WWATCH (zapisy)
    let mut stack_dumps = 0u32;
    // Precyzyjny shadow call-stack (push na BL, pop na powrocie). Wymaga CALLS=1.
    let mut callstack: Vec<(u32, u32)> = Vec::new();
    let mut usp_bad_seen = false;
    let mut halt_seen = false;
    let mut chk_seen = false;
    let mut txt_log_cnt = 0u32;
    // Eksperyment: wymus kod powodu wlaczenia (r4) na 0x2EF924 (przed cmp r4,#2/3/4),
    // by ominac sciezke resetu. FORCE_REASON=2 => klawisz POWER.
    let force_reason = std::env::var("FORCE_REASON").ok().and_then(|s| s.parse::<u32>().ok());
    // RAM-aware: histogram regionow wykonywanego PC + breakpoint na realnym adresie.
    // EXEC_BP=addr: zrzuca ctx gdy WYKONYWANY pc==addr (niezaleznie ROM/RAM).
    let exec_bp = std::env::var("EXEC_BP").ok().and_then(|s| parse_hex(&s));
    // Opcjonalny warunek: EXEC_BP_REG=N, EXEC_BP_VAL=hex -> lap tylko gdy r[N]==VAL.
    let exec_bp_reg = std::env::var("EXEC_BP_REG").ok().and_then(|s| s.parse::<usize>().ok());
    let exec_bp_val = std::env::var("EXEC_BP_VAL").ok().and_then(|s| parse_hex(&s));
    // EXEC_BP_AFTER=krok: lap breakpoint dopiero PO tym kroku (pomija wczesne hot-trafienia,
    // np. by zlapac porownanie SIMLOCK PO wpisaniu PIN, nie ATR-compare przy boocie).
    let exec_bp_after: u64 = std::env::var("EXEC_BP_AFTER").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    let mut exec_bp_cnt = 0u32;
    let mut cond7_cnt = 0u32; // licznik sondy COND7
    let mut pc_ram = 0u64;   // PC w RAM (<0x200000, poza wektorami)
    let mut pc_rom = 0u64;   // PC w ROM (>=0x200000)
    let mut pc_low = 0u64;   // PC w wektorach/niskie (<0x1000)
    // PC_HIST=1: histogram odwiedzin PC w bucketach co 0x10000 (0..0x400000 = 64 kosze).
    // Rozstrzyga ktore moduly sie WYKONUJA (np. 0x28=test executor/task3 self-test).
    let pc_hist_on = std::env::var("PC_HIST").is_ok();
    // PC_HIST_FROM=krok: zacznij histogram dopiero po tym kroku (lokalizacja kodu po PIN).
    let pc_hist_from: u64 = std::env::var("PC_HIST_FROM").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    let mut pc_hist = vec![0u64; 64];
    // FINE=lo:hi (hex) -> histogram drobny (bucket 0x40) w [lo,hi). Lokalizuje petle.
    let fine: Option<(u64, u64)> = std::env::var("FINE").ok().and_then(|s| {
        let mut it = s.splitn(2, ':');
        Some((parse_hex(it.next()?)? as u64, parse_hex(it.next()?)? as u64))
    });
    let mut fine_hist: Vec<u64> = fine.map(|(lo, hi)| vec![0u64; (((hi - lo) >> 6) + 1) as usize]).unwrap_or_default();
    // LASTPC=lo:hi (hex) -> zapamietuje OSTATNI krok z pc w [lo,hi) (punkt blokady taska).
    let lastpc: Option<(u64, u64)> = std::env::var("LASTPC").ok().and_then(|s| {
        let mut it = s.splitn(2, ':');
        Some((parse_hex(it.next()?)? as u64, parse_hex(it.next()?)? as u64))
    });
    let mut lastpc_seen: Option<(u32, u64)> = None;
    for i in 0..steps {
        let pc = cpu.get_next_pc();
        // Klasyfikacja regionu wykonywanego PC.
        if pc < 0x1000 { pc_low += 1; }
        else if pc < 0x0020_0000 { pc_ram += 1; }
        else { pc_rom += 1; }
        if pc_hist_on && i >= pc_hist_from { let b = (pc >> 16) as usize; if b < 64 { pc_hist[b] += 1; } }
        if let Some((lo, hi)) = fine { let p = pc as u64; if p >= lo && p < hi { fine_hist[((p - lo) >> 6) as usize] += 1; } }
        if let Some((lo, hi)) = lastpc { let p = pc as u64; if p >= lo && p < hi { lastpc_seen = Some((pc, i)); } }
        // Breakpoint na realnie wykonywanym adresie (dziala tez dla kodu w RAM).
        // Opcjonalny warunek na rejestrze (EXEC_BP_REG/VAL) - precyzyjne lapanie.
        let bp_cond = match (exec_bp_reg, exec_bp_val) {
            (Some(r), Some(v)) => cpu.get_reg(r) == v,
            _ => true,
        };
        if Some(pc) == exec_bp && bp_cond && exec_bp_cnt < 40 && i >= exec_bp_after {
            exec_bp_cnt += 1;
            print!("[EXEC_BP @{pc:#08X} #{exec_bp_cnt} krok {i}] r0..r12:");
            for r in 0..=12 { print!(" {:08X}", cpu.get_reg(r)); }
            print!(" lr={:08X} I={} F={} mode={:?}", cpu.get_reg(14),
                cpu.cpsr.irq_disabled() as u8, cpu.cpsr.fiq_disabled() as u8, cpu.cpsr.mode());
            // Opcjonalnie zrzuc pamiec spod adresu w rejestrze EXEC_BP_MEM (hex=nr rejestru).
            if let Some(mr) = std::env::var("EXEC_BP_MEM").ok().and_then(|s| s.parse::<usize>().ok()) {
                let base = cpu.get_reg(mr);
                print!(" | mem[r{mr}={base:08X}]:");
                for k in 0..0x18u32 { print!(" {:02X}", cpu.bus.debug_read_8(base + k)); }
            }
            println!();
        }
        // COND7: sonda 7 warunkow glownej petli bootu (fcn.002e1a08). Na kazdym
        // `cmp r0,0` loguje ktory warunek i jego r0. Pozwala ustalic ktory subsystem
        // nie zglasza gotowosci. Limit ~21 trafien (3 iteracje x 7).
        if std::env::var("COND7").is_ok() {
            let idx = match pc {
                0x002E_1A76 => Some(1u32), 0x002E_1A7E => Some(2),
                0x002E_1A86 => Some(3), 0x002E_1A8E => Some(4),
                0x002E_1A96 => Some(5), 0x002E_1A9E => Some(6),
                0x002E_1AA6 => Some(7), _ => None,
            };
            if let Some(n) = idx {
                if cond7_cnt < 21 {
                    cond7_cnt += 1;
                    println!("[COND7 #{n} @{pc:#08X} krok {i}] r0={:08X}", cpu.get_reg(0));
                }
            }
        }
        if let Some(v) = force_reason {
            if pc == 0x002E_F924 {
                cpu.set_reg(4, v); // wymus powod wlaczenia
            }
            if pc == 0x002E_F93E {
                cpu.set_reg(0, 0); // wymus "walidacja OK" (r0=0) po handlerze 0xF0124
            }
        }
        // SIM_ACCEPT (eksperyment): handler SIM-MMI 0x2df51c dostaje param_1=0x5E2 (offset 0x128
        // = REJECT, else=no-op). Wymus 0x5E1 (offset 0x127 = ACCEPT) -> FUN_002726be (SIM-ready)
        // + post 0x32c (kaskada standby). Test czy odblokowuje przejscie do standby/menu.
        if std::env::var("SIM_ACCEPT").is_ok() && pc == 0x002D_F51C && cpu.get_reg(0) == 0x5E2 {
            cpu.set_reg(0, 0x5E1);
        }
        // Eksperyment FORCE_BOOT: wymus r0=1 po kazdej z 7 funkcji warunku glownej petli
        // bootu (0x2e1a76..0x2e1aa6 cmp r0,0). Pozwala przejsc warunek -> zobaczyc co za petla.
        if std::env::var("FORCE_BOOT").is_ok() {
            if matches!(pc, 0x002E_1A76 | 0x002E_1A7E | 0x002E_1A86 | 0x002E_1A8E | 0x002E_1A96 | 0x002E_1A9E | 0x002E_1AA6) {
                cpu.set_reg(0, 1);
            }
        }
        // KEY_AT=krok: od tego kroku wstrzykuj cyklicznie nacisniecia klawisza (press 200k /
        // release 200k). KEY_CODE=hex (domyslnie KEY_UP 0x16); np. 0x20=MENU/Enter (zamyka
        // animacje), 0x21=Cancel. KEY_ONCE=1 -> tylko jedno nacisniecie (nie cyklicznie).
        if let Ok(kat) = std::env::var("KEY_AT") {
            if let Ok(k) = kat.parse::<u64>() {
                let code = std::env::var("KEY_CODE").ok().and_then(|s| parse_hex(&s))
                    .map(|v| v as u8).unwrap_or(emucore::keypad::KEY_UP);
                let once = std::env::var("KEY_ONCE").is_ok();
                if i >= k && !(once && i >= k + 400000) {
                    let phase = (i - k) % 400000;
                    if phase == 0 { cpu.bus.keypad.press_code(code); }
                    if phase == 200000 { cpu.bus.keypad.release_code(code); }
                }
            }
        }
        // KEY_SEQ="kod:krok,kod:krok,..." - sekwencja nacisniec (kazdy trzymany 150k krokow).
        // Pozwala wpisac PIN + przetestowac klawisz potwierdzenia (np. wejscie do menu).
        if let Ok(seq) = std::env::var("KEY_SEQ") {
            for part in seq.split(',') {
                let mut it = part.split(':');
                if let (Some(c), Some(s)) = (
                    it.next().and_then(parse_hex),
                    it.next().and_then(|x| x.parse::<u64>().ok()),
                ) {
                    if i == s { cpu.bus.keypad.press_code(c as u8); }
                    if i == s + 150000 { cpu.bus.keypad.release_code(c as u8); }
                }
            }
        }
        // Eksperyment POWER+stan: w petli oczekiwania 0x2ef9b6 wymus r5=1 (mock komunikatu
        // "stan bootu=1") ORAZ wcisnij POWER (zwiera bit kol1 -> skan zwraca 0x81),
        // by firmware przeszedl bramke i ruszyl dalej. Tylko gdy FORCE_R5 ustawione.
        if std::env::var("FORCE_R5").is_ok() && pc == 0x002E_F9B6 {
            cpu.set_reg(5, 1);
            cpu.bus.keypad.set_power(true);
        }
        // POWER zwolnij w kpd_wait_release (0x338d68) - inaczej blokuje przejscie.
        if std::env::var("FORCE_R5").is_ok() && pc == 0x0033_8D68 {
            cpu.bus.keypad.set_power(false);
        }
        // Detektor wejscia w halt 0x2EF95C: pokaz ZA PIERWSZYM razem skad i z czym.
        if pc == 0x002E_F95C && !halt_seen {
            halt_seen = true;
            print!("[HALT 0x2EF95C krok {i}] r0..r5:");
            for r in 0..=5 { print!(" {:08X}", cpu.get_reg(r)); }
            print!(" | sciezka:");
            for k in 0..24 { print!(" {:06X}", recent[(ridx + k) % 24] & 0xFFFFFF); }
            println!();
        }
        if pc == 0x002E_F924 && !chk_seen { chk_seen = true; println!("[CHK 0x2EF924 krok {i}] r4={:08X}", cpu.get_reg(4)); }
        // Wejscie w funkcje renderujaca tekst (0x283c0e) - pokaz sciezke + r0..r3
        // (r0 zwykle = ID/wskaznik tekstu). Pozwala namierzyc kto rysuje CONTACT.
        if pc == 0x0028_3C0E && txt_log_cnt < 12 {
            txt_log_cnt += 1;
            print!("[TXT 0x283C0E #{txt_log_cnt} krok {i}] r0..r3:");
            for r in 0..=3 { print!(" {:08X}", cpu.get_reg(r)); }
            print!(" | sciezka:");
            for k in 0..24 { print!(" {:06X}", recent[(ridx + k) % 24] & 0xFFFFFF); }
            println!();
        }

        // Wykryj, czy biezaca instrukcja to wywolanie (BL).
        let is_call = if trace_calls {
            match cpu.get_cpu_state() {
                arm7tdmi::CpuState::THUMB => {
                    let h = u16::from_be_bytes([cpu.bus.debug_read_8(pc), cpu.bus.debug_read_8(pc + 1)]);
                    (h & 0xF800) == 0xF800 // BL suffix
                }
                arm7tdmi::CpuState::ARM => {
                    let w = u32::from_be_bytes([
                        cpu.bus.debug_read_8(pc),
                        cpu.bus.debug_read_8(pc + 1),
                        cpu.bus.debug_read_8(pc + 2),
                        cpu.bus.debug_read_8(pc + 3),
                    ]);
                    ((w >> 24) & 0x0F) == 0x0B // BL
                }
            }
        } else {
            false
        };
        recent[ridx % 24] = pc;
        ridx += 1;
        let _ = (bank_was_set, &mut last_fiq_bank, &mut bank_chg_cnt);
        // Wykryj pierwszy moment, gdy sp watku User wchodzi w zly region (>0x11D800,
        // tj. w obszar stosow FIQ/IRQ 0x11E000+). To korupcja sp watku.
        if !usp_bad_seen && cpu.cpsr.mode() == arm7tdmi::CpuMode::User {
            let usp = cpu.get_reg(13);
            if (0x0011_D800..0x0011_FFFF).contains(&usp) {
                usp_bad_seen = true;
                print!("[USP-ZLY krok {i}] User sp={usp:08X} @PC={:06X} | sciezka:", pc & 0xFFFFFF);
                for k in 0..24 { print!(" {:06X}", recent[(ridx + k) % 24] & 0xFFFFFF); }
                println!();
            }
        }
        if clear_step.is_none() && cpu.bus.lcd.data_writes >= 504 {
            clear_step = Some(i);
        }
        // Co 50k krokow: zlap klatke o maksymalnej liczbie zapalonych pikseli.
        if i % 50_000 == 0 && i > std::env::var("PEAK_FROM").ok().and_then(|s| s.parse().ok()).unwrap_or(0u64) {
            let mut lit = 0usize;
            for y in 0..emucore::lcd::HEIGHT {
                for x in 0..emucore::lcd::WIDTH {
                    if cpu.bus.lcd.get(x, y) {
                        lit += 1;
                    }
                }
            }
            if lit > peak_lit {
                peak_lit = lit;
                peak_ascii = cpu.bus.lcd.to_ascii();
                peak_step = i;
            }
            // Zatrzask ostatniej NIEPUSTEJ klatki o "tekstowej" liczbie pikseli
            // (50..3500 - nie pelny ekran logo/animacji), jak robi GUI. To ekran PIN/menu.
            if (50..3500).contains(&lit) {
                last_text_ascii = cpu.bus.lcd.to_ascii();
                last_text_lit = lit;
                last_text_step = i;
            }
        }
        // Zrzut kontekstu przy pierwszym wejsciu w podejrzana petle hang.
        if pc == 0x002E_F9B6 && !dumped {
            dumped = true;
            print!("\n[HANG @0x2EF9B6] sciezka:");
            for k in 0..24 {
                print!(" {:06X}", recent[(ridx + k) % 24] & 0xFFFFFF);
            }
            print!("\n[HANG] rejestry:");
            for r in 0..=12 {
                print!(" r{r}={:08X}", cpu.get_reg(r));
            }
            println!(" sp={:08X} lr={:08X}", cpu.get_reg(13), cpu.get_reg(14));
        }
        if i >= asm_from && i < asm_from + asm_count {
            // Dezasembluj swiadomie ARM/Thumb wg stanu CPU (firmware BE).
            match cpu.get_cpu_state() {
                arm7tdmi::CpuState::ARM => {
                    let raw = u32::from_be_bytes([
                        cpu.bus.debug_read_8(pc),
                        cpu.bus.debug_read_8(pc + 1),
                        cpu.bus.debug_read_8(pc + 2),
                        cpu.bus.debug_read_8(pc + 3),
                    ]);
                    println!("  A {}", disasm(pc, raw));
                }
                arm7tdmi::CpuState::THUMB => {
                    let half = u16::from_be_bytes([
                        cpu.bus.debug_read_8(pc),
                        cpu.bus.debug_read_8(pc + 1),
                    ]);
                    println!("  T {}", disasm_thumb(pc, half));
                }
            }
        }
        // Wykryj pierwszy skok w zakres "smieciowy" [0x1000,0x100000) = mis-jump watku.
        if escaped_at.is_none() && (0x0000_1000..0x0010_0000).contains(&pc) {
            escaped_at = Some((i, pc));
            print!("\n[MIS-JUMP @{pc:#08X} krok {i}] sciezka:");
            for k in 0..24 {
                print!(" {:06X}", recent[(ridx + k) % 24] & 0xFFFFFF);
            }
            print!("\n[MIS-JUMP] regs:");
            for r in 0..=15 {
                print!(" r{r}={:08X}", cpu.get_reg(r));
            }
            println!();
        }
        if Some(pc) == dump_pc && dump_cnt < 8 {
            dump_cnt += 1;
            print!("[DUMP @{pc:#08X} #{dump_cnt}] sciezka:");
            for k in 0..24 {
                print!(" {:06X}", recent[(ridx + k) % 24] & 0xFFFFFF);
            }
            print!(" | regs:");
            for r in 0..=15 {
                print!(" r{r}={:08X}", cpu.get_reg(r));
            }
            println!();
        }
        // Detektor skoku w bardzo niski adres = wywolanie NULL/uszkodzony powrot.
        // Lap PC==0 (NULL) oraz 0x20..0x1000; ignoruj wektory wyjatkow 0x04..0x1F (legalne).
        if (pc == 0 || (pc >= 0x20 && pc < 0x0000_1000)) && !lowjump_seen {
            lowjump_seen = true;
            let sp = cpu.get_reg(13);
            print!("[LOWJUMP @{pc:#08X} krok {i}] tryb={:?} sp={sp:#08X} lr={:#08X} | stos@sp:", cpu.cpsr.mode(), cpu.get_reg(14));
            for k in 0..10 {
                let a = sp.wrapping_add(k * 4);
                let w = u32::from_be_bytes([cpu.bus.debug_read_8(a), cpu.bus.debug_read_8(a+1), cpu.bus.debug_read_8(a+2), cpu.bus.debug_read_8(a+3)]);
                print!(" {w:08X}");
            }
            print!(" | sciezka:");
            for k in 0..24 { print!(" {:06X}", recent[(ridx + k) % 24] & 0xFFFFFF); }
            println!();
        }
        cpu.bus.pc_hint = pc;
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cpu.step())).is_err() {
            println!("\n*** PANIKA rdzenia na PC={pc:#010X} (krok {i}) ***");
            print!("sciezka:");
            for k in 0..24 {
                print!(" {:06X}", recent[(ridx + k) % 24] & 0xFFFFFF);
            }
            println!();
            break;
        }

        if is_call {
            let target = cpu.get_next_pc();
            *call_count.entry(target).or_insert(0) += 1;
            // shadow stack: zapamietaj (miejsce wywolania, adres powrotu = lr po BL)
            callstack.push((pc, cpu.get_reg(14) & !1));
        } else if let Some(&(_, ret)) = callstack.last() {
            // powrot: pc skoczyl na zapisany adres powrotu -> zdejmij ramke
            if (cpu.get_next_pc() & !1) == ret {
                callstack.pop();
            }
        }
        // Zrzut PRECYZYJNEGO shadow-stacku gdy watch (odczyt) LUB wwatch (zapis) trafiony.
        if cpu.bus.watch_hits.len() > watch_prev || cpu.bus.wwatch_hits.len() > wwatch_prev {
            watch_prev = cpu.bus.watch_hits.len();
            wwatch_prev = cpu.bus.wwatch_hits.len();
            if stack_dumps < 4 && i > 71_000_000 {
                stack_dumps += 1;
                println!("\n[CALLSTACK @krok {i}] pc={pc:#08X} - lancuch wywolan (gora=najglebszy/ostatni):");
                print!("    rejestry:");
                for r in 0..=12 { print!(" r{r}={:08X}", cpu.get_reg(r)); }
                println!();
                let n = callstack.len();
                for (k, (cs, ret)) in callstack.iter().enumerate().rev() {
                    if n - k <= 30 {
                        println!("    #{} call@{cs:#08X} -> ret {ret:#08X}", n - k);
                    }
                }
            }
        }

        // Programowy reset CPU (firmware zapisal bit2 do IO_CTSI_RST 0x20001).
        if cpu.bus.reset_request {
            cpu.bus.reset_request = false;
            cpu.bus.reset_count += 1;
            if reset_log_cnt < 10 {
                reset_log_cnt += 1;
                println!("[RESET #{} krok {i}] PC={pc:#08X} -> restart od FW_ENTRY", cpu.bus.reset_count);
            }
            cpu.reset();
            cpu.set_reg(15, emucore::machine::FW_ENTRY);
            cpu.reload_pipeline32();
            continue;
        }

        // Timer CTSI + asercja przerwan (FIQ ma pierwszenstwo).
        cpu.bus.tick_timer();
        if irq_period != u64::MAX {
            if cpu.bus.fiq_active() {
                let before_mode = cpu.cpsr.mode();
                let before_pc = cpu.get_next_pc();
                let before_sp = cpu.get_reg(13);
                cpu.fiq();
                if fiq_log_cnt < 6 {
                    fiq_log_cnt += 1;
                    println!("[FIQ #{fiq_log_cnt} krok {i}] przed: tryb={before_mode:?} pc={before_pc:#08X} sp={before_sp:#08X} | po: tryb={:?} sp_fiq={:#08X} lr_fiq={:#08X} | bank_r13={:08X?}",
                        cpu.cpsr.mode(), cpu.get_reg(13), cpu.get_reg(14), cpu.banks.gpr_banked_r13);
                }
            } else if cpu.bus.irq_active() || cpu.bus.take_irq_tick() {
                cpu.irq();
            }
        }
    }

    if !cpu.bus.wwatch_hits.is_empty() {
        let a = cpu.bus.wwatch.unwrap_or(0);
        println!("\n--- WWATCH {a:#010X}: zapisy (PC -> wartosc), pierwsze {} ---", cpu.bus.wwatch_hits.len());
        for (pc, v) in cpu.bus.wwatch_hits.iter() {
            println!("   PC={pc:#010X} <- {v:#04X}");
        }
    }

    // Skopiuj dane diagnostyczne z magistrali (zwalnia borrow przed odczytami mut).
    let (mmio_r, mmio_w, rom_w, unmapped, watch_hits, hot, trace_log) = {
        let m = &*cpu.bus;
        (
            m.mmio_reads,
            m.mmio_writes,
            m.rom_writes,
            m.unmapped,
            m.watch_hits.clone(),
            m.hot_reads(8),
            m.trace.clone(),
        )
    };

    // Stan LCD (PCD8544).
    let (lcd_ascii, lcd_data_w, lcd_cmd_w, lcd_lit, lcd_nz, lcd_cmds) = {
        let m = &*cpu.bus;
        (
            m.lcd.to_ascii(),
            m.lcd.data_writes,
            m.lcd.cmd_writes,
            m.lcd.any_lit(),
            m.lcd.nonzero_data,
            m.lcd.cmd_log.clone(),
        )
    };

    println!("\n=== Wynik ===");
    println!("Wykonanie PC: ROM={pc_rom} RAM={pc_ram} niskie/wektory={pc_low}");
    println!("PC koncowy        : {:#010X}", cpu.get_next_pc());
    println!("Clear LCD (504B) na kroku: {clear_step:?}");
    println!("PEAK ekran: {peak_lit} pikseli @krok {peak_step}\n{peak_ascii}");
    println!("KONCOWY ekran (lit={lcd_lit}):\n{lcd_ascii}");
    println!("OSTATNI ekran tekstowy ({last_text_lit} lit @krok {last_text_step}):\n{last_text_ascii}");
    print!("Ostatnie 24 PC   :");
    for k in 0..24 {
        print!(" {:06X}", recent[(ridx + k) % 24] & 0xFFFFFF);
    }
    println!();
    // Tablica wektorow wyjatkow 0x00..0x20 (BE) - czy IRQ@0x18 zainstalowany?
    print!("Wektory 0x00..0x20:");
    for a in (0..0x20u32).step_by(4) {
        let w = u32::from_be_bytes([
            cpu.bus.debug_read_8(a),
            cpu.bus.debug_read_8(a + 1),
            cpu.bus.debug_read_8(a + 2),
            cpu.bus.debug_read_8(a + 3),
        ]);
        print!(" [{a:02X}]={w:08X}");
    }
    println!();
    if pc_hist_on {
        println!("PC_HIST (bucket 0x10000, niezerowe):");
        for (b, &c) in pc_hist.iter().enumerate() {
            if c > 0 { println!("  {:#08X}..: {c}", (b as u32) << 16); }
        }
    }
    // (diagnostyka tasków: patrz memory new-firmware-v639 - task 3 dormant stan 5)
    if lastpc.is_some() {
        match lastpc_seen {
            Some((p, s)) => println!("LASTPC w zakresie: {p:#08X} @krok {s}"),
            None => println!("LASTPC: zakres nigdy nie odwiedzony"),
        }
    }
    if let Some((lo, _hi)) = fine {
        println!("FINE_HIST (bucket 0x40, niezerowe, top wg liczby):");
        let mut v: Vec<(usize, u64)> = fine_hist.iter().copied().enumerate().filter(|(_, c)| *c > 0).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        for (i, c) in v.iter().take(25) { println!("  {:#08X}: {c}", lo + ((*i as u64) << 6)); }
    }
    // MEMDUMP=addr[:n] -> zrzut n bajtow RAM/ROM na koniec (diagnostyka tablic/flag).
    if let Ok(md) = std::env::var("MEMDUMP") {
        let mut it = md.splitn(2, ':');
        if let Some(a) = it.next().and_then(parse_hex) {
            let n = it.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(32);
            print!("MEMDUMP {a:#010X}:");
            for k in 0..n { print!(" {:02X}", cpu.bus.debug_read_8(a + k)); }
            println!();
        }
    }
    println!(
        "LCD: dane={lcd_data_w} (niezerowych={lcd_nz}), komend={lcd_cmd_w}, zapalone={lcd_lit}"
    );
    println!(
        "LCD rutyna: last_cmd_pc={:#010X}, last_data_pc={:#010X}",
        cpu.bus.lcd_last_cmd_pc, cpu.bus.lcd_last_data_pc
    );
    println!(
        "KEYPAD: odczyty KPD_C={}, z wykrytym klawiszem={}, asercje IRQ bit0={}",
        cpu.bus.keypad.read_c_count.get(),
        cpu.bus.keypad.read_c_hit.get(),
        cpu.bus.key_irq_asserts
    );
    println!(
        "CTSI koncowy: irq_en={} irq_mask={:#04X} irq_latch={:#04X} fiq_en={} fiq_mask={:#04X} fiq_latch={:#04X}",
        cpu.bus.ctsi.irq_en, cpu.bus.ctsi.irq_mask, cpu.bus.ctsi.irq_latch,
        cpu.bus.ctsi.fiq_en, cpu.bus.ctsi.fiq_mask, cpu.bus.ctsi.fiq_latch
    );
    print!("CCONT odczyty rejestrow:");
    for r in 0..0x10 {
        if cpu.bus.ccont.read_count[r] > 0 {
            print!(" [{:X}]r{}", r, cpu.bus.ccont.read_count[r]);
        }
    }
    print!("  | zapisy:");
    for r in 0..0x10 {
        if cpu.bus.ccont.write_count[r] > 0 {
            print!(" [{:X}]w{}", r, cpu.bus.ccont.write_count[r]);
        }
    }
    println!();
    print!("LCD komendy:");
    for c in lcd_cmds.iter().take(40) {
        print!(" {c:02X}");
    }
    println!();
    if lcd_data_w > 0 {
        println!("\n--- EKRAN LCD (84x48) ---\n{lcd_ascii}");
    }
    match escaped_at {
        Some((i, pc)) => println!("PC opuscil ROM    : krok {i}, PC={pc:#010X}"),
        None => println!("PC opuscil ROM    : nie (cały czas w ROM) ✔"),
    }
    println!(
        "MMIO  R/W         : {} / {}   (okno {:#X}..{:#X})",
        mmio_r, mmio_w, MMIO_START, MMIO_END
    );
    println!("proby zapisu ROM  : {rom_w}");
    println!("dostepy nieznane  : {unmapped}");

    // --- Watch: kto i z czym czyta obserwowany adres ---
    if !watch_hits.is_empty() {
        println!("\n--- WATCH {watch:#010X}: pierwsze odczyty (PC, szer, wartosc) ---");
        for (pc, w, val) in watch_hits.iter().take(8) {
            println!("   PC={pc:#010X}  u{w}  -> {val:#06X}");
        }
        // Dezasembluj (Thumb) region petli wokol PC pierwszego trafienia.
        let poll_pc = watch_hits[0].0 & !1;
        println!("\n--- Dezasemblacja (Thumb) petli pollingu wokol PC={poll_pc:#010X} ---");
        let start = poll_pc.saturating_sub(12);
        let mut a = start;
        while a < poll_pc + 16 {
            let half =
                u16::from_be_bytes([cpu.bus.debug_read_8(a), cpu.bus.debug_read_8(a + 1)]);
            let mark = if a == poll_pc { " <== poll" } else { "" };
            println!("  {}{}", disasm_thumb(a, half), mark);
            a += 2;
        }
    }

    if trace_calls {
        let mut calls: Vec<_> = call_count.iter().map(|(a, c)| (*a, *c)).collect();
        calls.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
        println!("\n--- Najczesciej wolane funkcje (BL target x licznik) ---");
        for (a, c) in calls.iter().take(30) {
            println!("   {a:#010X}  x{c}");
        }
    }

    println!("\n--- Najczesciej czytane adresy MMIO (busy-poll?) ---");
    for (addr, cnt) in hot {
        let off = addr.wrapping_sub(emucore::machine::IO_BASE) & 0xFFFF;
        let note = if (addr & 0xFFFF_0000) == emucore::machine::IO_BASE {
            format!("IO_BASE+{:#04X}", off)
        } else {
            String::from("(poza CTSI)")
        };
        println!("   {addr:#010X}  x{cnt:<10}  {note}");
    }

    println!("\n--- Log pierwszych dostepow do MMIO/nieznanych ({} wpisow) ---", trace_log.len());
    println!("   {:>10}  {:<3} {:<5} {:>10}  {:>10}", "PC", "R/W", "szer", "addr", "value");
    for a in &trace_log {
        let reg = a.addr.wrapping_sub(emucore::machine::IO_BASE);
        let note = if (a.addr & 0xFFFF_0000) == emucore::machine::IO_BASE {
            format!("  ; IO_BASE+{:#04X}", reg & 0xFFFF)
        } else {
            String::new()
        };
        println!(
            "   {:#010X}  {:<3} u{:<4} {:#010X}  {:#010X}{}",
            a.pc,
            if a.write { "W" } else { "R" },
            a.width,
            a.addr,
            a.value,
            note
        );
    }
}
