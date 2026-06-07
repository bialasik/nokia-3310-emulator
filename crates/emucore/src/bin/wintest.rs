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
    while done < total {
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
    dump(&emu, &format!("krok {done}"));
    let (kr, kh, ka) = emu.key_diag();
    println!("battery_mv={} clock={:?} crashed={} | KEYPAD: KPD_C odczyty={kr} z_klawiszem={kh} IRQ_asercje={ka}",
        emu.battery_mv(), emu.clock(), emu.crashed);
}
