//! Headless harness odtwarzajacy SCIEZKE OKNA (emu.rs Emulator), nie trace.rs.
//! Uruchamia emulator jak frontend, opcjonalnie wstrzykuje Enter (Select) w zadanym
//! kroku, i zrzuca ekran LCD ASCII w punktach kontrolnych. Pozwala odtworzyc scenariusz
//! uzytkownika (animacja -> Enter -> etap po niej) dokladnie tak jak okno.
//!
//! Uzycie: wintest <total_steps> [enter_at_step]
//!   env jak RUN_STOCK: FORCE_R5=1 DSP_FIQ_AT=50000 FORCE_REASON=2 FORCE_ST=1

use emucore::{EmuKey, Emulator};

fn dump(emu: &Emulator, label: &str) {
    println!("=== EKRAN @{label} (pc={:#08X}) ===", emu.pc());
    let mut any = false;
    for y in 0..emucore::emu::LCD_H {
        let mut line = String::with_capacity(emucore::emu::LCD_W + 2);
        line.push('|');
        for x in 0..emucore::emu::LCD_W {
            let on = emu.lcd_get(x, y);
            if on { any = true; }
            line.push(if on { '#' } else { ' ' });
        }
        line.push('|');
        println!("{line}");
    }
    println!("(cokolwiek zapalone: {any})");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let total: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(150_000_000);
    let enter_at: Option<u64> = args.get(2).and_then(|s| s.parse().ok());

    let mut emu = match Emulator::new() {
        Ok(e) => e,
        Err(e) => { eprintln!("blad: {e}"); std::process::exit(1); }
    };
    println!("firmware: {}", emu.firmware_id);

    let chunk: u64 = std::env::var("CHUNK").ok().and_then(|s| s.parse().ok()).unwrap_or(1_000_000);
    let CHUNK = chunk;
    let mut done = 0u64;
    let mut enter_pressed = false;
    let mut enter_released = false;
    // PIN entry: env PIN="1234" wpisywane od kroku PIN_AT (domyslnie 22M), kazda cyfra
    // trzymana 1.5M, odstep 3M; po ostatniej cyfrze Select (OK) by potwierdzic -> reject/menu.
    let pin: Vec<u8> = std::env::var("PIN").ok().map(|s| s.bytes().filter(|b| b.is_ascii_digit()).map(|b| b - b'0').collect()).unwrap_or_default();
    let pin_at: u64 = std::env::var("PIN_AT").ok().and_then(|s| s.parse().ok()).unwrap_or(22_000_000);
    let mut pin_phase = vec![false; pin.len() * 2 + 2]; // press/release per cyfra + press/release OK
    // POST_KEYS="select:36000000,down:38000000,...": klawisze PO PIN (test menu z ekranu reject).
    // Nazwy: select(=Menu), up, down, back, c. Kazdy trzymany 1.5M.
    let post_keys: Vec<(EmuKey, u64)> = std::env::var("POST_KEYS").ok().map(|s| s.split(',').filter_map(|p| {
        let mut it = p.split(':');
        let name = it.next()?;
        let step: u64 = it.next()?.parse().ok()?;
        let key = match name.trim() {
            "select" | "menu" => EmuKey::Select,
            "up" => EmuKey::Up,
            "down" => EmuKey::Down,
            "back" | "c" => EmuKey::Back,
            _ => return None,
        };
        Some((key, step))
    }).collect()).unwrap_or_default();
    let mut post_phase = vec![false; post_keys.len() * 2];
    // DUMP_AT="36000000,40000000": dodatkowe zrzuty ekranu w tych krokach.
    let dump_at: Vec<u64> = std::env::var("DUMP_AT").ok().map(|s| s.split(',').filter_map(|x| x.trim().parse().ok()).collect()).unwrap_or_default();
    let mut dumped: Vec<bool> = vec![false; dump_at.len()];
    while done < total {
        for (i, &(key, step)) in post_keys.iter().enumerate() {
            if !post_phase[i * 2] && done >= step {
                emu.set_key(key, true); post_phase[i * 2] = true;
                println!("[POST_KEY #{i} WCISNIETY @krok {done}]");
            }
            if !post_phase[i * 2 + 1] && done >= step + 1_500_000 {
                emu.set_key(key, false); post_phase[i * 2 + 1] = true;
            }
        }
        for (i, &d) in dump_at.iter().enumerate() {
            if !dumped[i] && done >= d { dumped[i] = true; dump(&emu, &format!("DUMP_AT krok {done}")); }
        }
        // Wstrzyknij Enter (Select) w zadanym kroku, zwolnij ~5M krokow pozniej.
        if let Some(ea) = enter_at {
            if !enter_pressed && done >= ea {
                emu.set_key(EmuKey::Select, true);
                enter_pressed = true;
                println!("[Enter WCISNIETY @krok {done}]");
            }
            if enter_pressed && !enter_released && done >= ea + 3_000_000 {
                emu.set_key(EmuKey::Select, false);
                enter_released = true;
                println!("[Enter ZWOLNIONY @krok {done}]");
            }
        }
        // PIN: sekwencja cyfr + OK. Cyfra i: press @pin_at+i*3M, release @+1.5M.
        if !pin.is_empty() {
            for (i, &d) in pin.iter().enumerate() {
                let t = pin_at + (i as u64) * 3_000_000;
                if !pin_phase[i * 2] && done >= t {
                    emu.set_key(EmuKey::Digit(d), true); pin_phase[i * 2] = true;
                    println!("[PIN cyfra {d} WCISNIETA @krok {done}]");
                }
                if !pin_phase[i * 2 + 1] && done >= t + 1_500_000 {
                    emu.set_key(EmuKey::Digit(d), false); pin_phase[i * 2 + 1] = true;
                }
            }
            // OK (Select) po wszystkich cyfrach.
            let ok_t = pin_at + (pin.len() as u64) * 3_000_000;
            let pl = pin.len() * 2;
            if !pin_phase[pl] && done >= ok_t {
                emu.set_key(EmuKey::Select, true); pin_phase[pl] = true;
                println!("[PIN OK (Select) WCISNIETY @krok {done}]");
            }
            if !pin_phase[pl + 1] && done >= ok_t + 1_500_000 {
                emu.set_key(EmuKey::Select, false); pin_phase[pl + 1] = true;
            }
        }
        // Odtworz petle klawiatury OKNA: set_key(Power,false) co "klatke" (jak main.rs).
        if std::env::var("GUI_KEYS").is_ok() {
            emu.set_key(EmuKey::Power, false);
        }
        emu.run_steps(CHUNK);
        done += CHUNK;
        if done % 20_000_000 == 0 {
            let (fe, fm, fl, im, t0, t0t, arm) = emu.ctsi_state();
            println!("@{:>4}M: pc={:#08X} lcd={} clk={:?} | FIQ en={} mask={:#04X} latch={:#04X} IRQmask={:#04X} TMR0={:#06X}/{:#06X} armed={}",
                done / 1_000_000, emu.pc(), emu.lcd_data_writes(), emu.clock().2,
                fe, fm, fl, im, t0, t0t, arm);
        }
    }
    if std::env::var("PCWIN").is_ok() {
        let mut v: Vec<(u32, u64)> = emu.pcwin_hist.iter().map(|(k, c)| (*k, *c)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        println!("=== PCWIN histogram (top 25 bucketow 0x100) ===");
        for (pc, c) in v.iter().take(25) {
            println!("  {pc:#08X}: {c}");
        }
    }
    dump(&emu, &format!("krok {done}"));
    let (kr, kh, ka) = emu.key_diag();
    println!("battery_mv={} clock={:?} crashed={} | KEYPAD: KPD_C odczyty={kr} z_klawiszem={kh} IRQ_asercje={ka}",
        emu.battery_mv(), emu.clock(), emu.crashed);
}
