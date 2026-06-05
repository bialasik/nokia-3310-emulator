//! Nokia 3310 - emulator (frontend / okno).
//!
//! Napedza prawdziwy firmware przez `emucore::Emulator` i renderuje bufor LCD
//! kontrolera PCD8544 (84x48) w oknie z klimatem monochromatycznego ekranu Nokii.
//! Gdy brak ROM-u w roms/ lub crates/rom/ -> tryb demo (atrapa menu).
//!
//! Sterowanie:
//!   Esc - wyjscie
//!   (mapowanie klawiatury na klawiature telefonu: M4)

mod font;
mod lcd;
mod ui;

use lcd::{Lcd, HEIGHT, WIDTH};
use minifb::{Key, KeyRepeat, Window, WindowOptions};
use std::time::Instant;
use ui::{Key as NokiaKey, Ui};

/// Zegar CPU oryginalnej Nokii 3310: ARM7TDMI @ 13 MHz.
const CLOCK_HZ: f64 = 13_000_000.0;

/// Ile pikseli okna na 1 piksel LCD.
const SCALE: usize = 6;
/// Margines wokol ekranu (obudowa telefonu).
const MARGIN: usize = 26;

/// Kolory (0x00RRGGBB).
const COL_OFF: u32 = 0x00A8_C0A0; // tlo LCD - blado-zielone
const COL_ON: u32 = 0x0020_2818; // piksel zapalony - ciemny
const COL_FRAME: u32 = 0x0012_386B; // obudowa - granat Nokii
const COL_BEZEL: u32 = 0x0008_0C08; // czarna ramka wokol szkla

const WIN_W: usize = WIDTH * SCALE + 2 * MARGIN;
const WIN_H: usize = HEIGHT * SCALE + 2 * MARGIN;

fn main() {
    let mut window = Window::new("Nokia 3310 - emulator", WIN_W, WIN_H, WindowOptions::default())
        .expect("nie udalo sie utworzyc okna");
    window.set_target_fps(30);

    let mut lcd = Lcd::new();
    let mut framebuf = vec![0u32; WIN_W * WIN_H];

    // Sprobuj uruchomic prawdziwy firmware; inaczej tryb demo.
    let mut emu = match emucore::Emulator::new() {
        Ok(e) => {
            println!("[emu] firmware: {}", e.firmware_id);
            Some(e)
        }
        Err(e) => {
            eprintln!("[emu] {e} - tryb demo");
            None
        }
    };

    let mut demo = Ui::new(None);
    let mut frame: u64 = 0;
    // Stan detekcji krawedzi klawiszy (per logiczny klawisz): czy byl fizycznie wcisniety
    // w poprzedniej klatce + ile klatek jeszcze trzymac emulowane nacisniecie. Cel: tap =
    // JEDNO krotkie nacisniecie, BEZ auto-repeat firmware przy trzymaniu (nawigacja/menu).
    let mut key_was_down = [false; 16];
    let mut key_press_left = [0u8; 16];

    // Pacing do prawdziwego zegara 13 MHz: budzet krokow rosnie z czasem sciany,
    // z limitem (nie nadrabiamy wiecej niz 50 ms naraz - brak spirali smierci).
    let mut last = Instant::now();
    let mut budget = 0.0f64;
    let max_batch = CLOCK_HZ * 0.05;
    // Pomiar efektywnej predkosci (okno 1 s).
    let mut spd_t = Instant::now();
    let mut spd_steps0 = 0u64;
    let mut eff_mhz = 0.0f64;

    while window.is_open() && !window.is_key_down(Key::Escape) {
        let now = Instant::now();
        let dt = (now - last).as_secs_f64();
        last = now;

        match emu.as_mut() {
            Some(e) => {
                // Ile krokow "nalezy sie" za uplyniony czas (13 MHz).
                budget += dt * CLOCK_HZ;
                let n = budget.min(max_batch) as u64;
                budget -= n as f64;
                if budget > max_batch {
                    budget = max_batch; // nie kumuluj dlugu
                }
                // Wstrzyknij stan klawiszy (PC -> klawiatura telefonu). KRYTYCZNE: jeden
                // EmuKey moze miec WIELE klawiszy fizycznych (np. cyfra = rzad numeryczny
                // LUB numpad). Trzeba zsumowac (OR) ich stan i wywolac set_key DOKLADNIE
                // RAZ per EmuKey - inaczej druga mapa (niewcisniety numpad) robi release_code
                // tuz po press_code z pierwszej -> klawisz naciskany i zwalniany w tej samej
                // klatce (0->1->0), firmware nie zdazy go zarejestrowac (cyfry "nie dzialaja").
                let dn = |k: Key| window.is_key_down(k);
                // POWER osobno (model ma wlasny auto-hold ~2-3s przy starcie; potem stan fizyczny).
                e.set_key(emucore::EmuKey::Power, dn(Key::P));
                // Logiczne klawisze: (EmuKey, czy fizycznie wcisniety). Cyfra = rzad LUB numpad.
                let logical: [(emucore::EmuKey, bool); 16] = [
                    (emucore::EmuKey::Up, dn(Key::Up)),
                    (emucore::EmuKey::Down, dn(Key::Down)),
                    (emucore::EmuKey::Select, dn(Key::Enter)),
                    (emucore::EmuKey::Back, dn(Key::Backspace)),
                    (emucore::EmuKey::Star, dn(Key::LeftBracket)),
                    (emucore::EmuKey::Hash, dn(Key::RightBracket)),
                    (emucore::EmuKey::Digit(0), dn(Key::Key0) || dn(Key::NumPad0)),
                    (emucore::EmuKey::Digit(1), dn(Key::Key1) || dn(Key::NumPad1)),
                    (emucore::EmuKey::Digit(2), dn(Key::Key2) || dn(Key::NumPad2)),
                    (emucore::EmuKey::Digit(3), dn(Key::Key3) || dn(Key::NumPad3)),
                    (emucore::EmuKey::Digit(4), dn(Key::Key4) || dn(Key::NumPad4)),
                    (emucore::EmuKey::Digit(5), dn(Key::Key5) || dn(Key::NumPad5)),
                    (emucore::EmuKey::Digit(6), dn(Key::Key6) || dn(Key::NumPad6)),
                    (emucore::EmuKey::Digit(7), dn(Key::Key7) || dn(Key::NumPad7)),
                    (emucore::EmuKey::Digit(8), dn(Key::Key8) || dn(Key::NumPad8)),
                    (emucore::EmuKey::Digit(9), dn(Key::Key9) || dn(Key::NumPad9)),
                ];
                for (i, (ek, phys)) in logical.iter().enumerate() {
                    if *phys && !key_was_down[i] {
                        e.set_key(*ek, true); // krawedz narastajaca -> jedno nacisniecie
                        key_press_left[i] = 2; // trzymaj 2 klatki (firmware zdazy zarejestrowac)
                    } else if key_press_left[i] > 0 {
                        key_press_left[i] -= 1;
                        if key_press_left[i] == 0 {
                            e.set_key(*ek, false); // auto-zwolnij (nie czekaj na fizyczne puszczenie)
                        }
                    }
                    key_was_down[i] = *phys;
                }
                e.run_steps(n);
                if frame % 60 == 0 && std::env::var("GUI_DBG").is_ok() {
                    eprintln!("[gui] pc={:#08X} steps={} lcd_lit={}", e.pc(), e.total_steps, e.lcd_any_lit());
                }
                // Zatrzaskuj ostatnia NIEPUSTA klatke (tresc przetrwa transient/crash).
                if e.lcd_any_lit() {
                    for y in 0..HEIGHT {
                        for x in 0..WIDTH {
                            lcd.set(x as i32, y as i32, e.lcd_get(x, y));
                        }
                    }
                }
                // Efektywna predkosc co ~1 s.
                if spd_t.elapsed().as_secs_f64() >= 1.0 {
                    eff_mhz = (e.total_steps - spd_steps0) as f64
                        / spd_t.elapsed().as_secs_f64()
                        / 1.0e6;
                    spd_steps0 = e.total_steps;
                    spd_t = Instant::now();
                }
                if frame % 8 == 0 {
                    let status = if e.crashed { "STOP" } else { "running" };
                    window.set_title(&format!(
                        "Nokia 3310 [{}] - {:.2}s CPU - {:.1} MHz - PC={:#08X} - {}{}",
                        e.firmware_id,
                        e.total_steps as f64 / CLOCK_HZ,
                        eff_mhz,
                        e.pc(),
                        status,
                        if e.lcd_any_lit() { " - LCD!" } else { " - (pusty)" },
                    ));
                }
            }
            None => {
                // Tryb demo (atrapa) - reaguje na klawisze.
                for k in window.get_keys_pressed(KeyRepeat::No) {
                    let mapped = match k {
                        Key::Up => Some(NokiaKey::Up),
                        Key::Down => Some(NokiaKey::Down),
                        Key::Enter => Some(NokiaKey::Select),
                        Key::Backspace | Key::C => Some(NokiaKey::Back),
                        _ => None,
                    };
                    if let Some(nk) = mapped {
                        demo.on_key(nk);
                    }
                }
                demo.tick();
                demo.render(&mut lcd);
            }
        }

        blit(&lcd, &mut framebuf);
        window
            .update_with_buffer(&framebuf, WIN_W, WIN_H)
            .expect("blad aktualizacji okna");
        frame += 1;
    }
}

/// Przerysowuje bufor LCD 84x48 na bufor okna ze skalowaniem, ramka i bezelem.
fn blit(lcd: &Lcd, out: &mut [u32]) {
    out.fill(COL_FRAME);

    let bez = 4;
    for y in (MARGIN - bez)..(MARGIN + HEIGHT * SCALE + bez) {
        for x in (MARGIN - bez)..(MARGIN + WIDTH * SCALE + bez) {
            out[y * WIN_W + x] = COL_BEZEL;
        }
    }

    for sy in 0..HEIGHT {
        for sx in 0..WIDTH {
            let color = if lcd.get(sx, sy) { COL_ON } else { COL_OFF };
            let ox = MARGIN + sx * SCALE;
            let oy = MARGIN + sy * SCALE;
            for dy in 0..SCALE {
                let row = (oy + dy) * WIN_W + ox;
                for dx in 0..SCALE {
                    out[row + dx] = color;
                }
            }
        }
    }
}
