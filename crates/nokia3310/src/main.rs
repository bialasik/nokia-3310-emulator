//! Nokia 3310 - emulator (frontend / okno).
//!
//! Napedza prawdziwy firmware przez `emucore::Emulator` i renderuje bufor LCD
//! kontrolera PCD8544 (84x48) w oknie z klimatem monochromatycznego ekranu Nokii.
//! Gdy brak ROM-u w roms/ lub crates/rom/ -> tryb demo (atrapa menu).
//!
//! Sterowanie:
//!   Esc - wyjscie
//!   (mapowanie klawiatury na klawiature telefonu: M4)

mod audio;
mod font;
mod lcd;
mod ui;

use lcd::{Lcd, HEIGHT, WIDTH};
use minifb::{Key, KeyRepeat, Window, WindowOptions};
use std::time::Instant;
use ui::{Key as NokiaKey, Ui};

/// Zegar CPU oryginalnej Nokii 3310: ARM7TDMI @ 13 MHz (13M CYKLI/s).
const CLOCK_HZ: f64 = 13_000_000.0;
/// Estymata srednich cykli na instrukcje (wait-states flasha). Tylko do wyliczenia ile
/// instrukcji wykonac na klatke - po wykonaniu odejmujemy RZECZYWIScie zuzyte cykle, wiec
/// blad estymaty sam sie koryguje. Zmierzone ~3.3 dla v6.39 (FLASH_WS=2).
const CPI_EST: f64 = 3.3;

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
/// Panel wizualizacji klawiatury pod ekranem (podswietla wcisniete klawisze).
const PANEL_H: usize = 156;
const WIN_H: usize = HEIGHT * SCALE + 2 * MARGIN + PANEL_H;

/// Uklad panelu klawiszy: (etykieta, kolumna, wiersz). Kolejnosc = indeks w tablicy `down`.
const KEYPAD: [(&str, usize, usize); 17] = [
    ("MENU", 0, 0), ("UP", 1, 0), ("C", 2, 0),
    ("PWR", 0, 1), ("DOWN", 1, 1),
    ("1", 0, 2), ("2", 1, 2), ("3", 2, 2),
    ("4", 0, 3), ("5", 1, 3), ("6", 2, 3),
    ("7", 0, 4), ("8", 1, 4), ("9", 2, 4),
    ("*", 0, 5), ("0", 1, 5), ("#", 2, 5),
];
const COL_KEY: u32 = 0x0023_3F23; // klawisz niewcisniety (ciemny)
const COL_KEY_ON: u32 = 0x0030_E030; // klawisz WCISNIETY (jasny zielony)

fn main() {
    // GUI domyslnie liczy uplyw czasu CYKLAMI CPU (model wait-states) - realne tempo telefonu.
    // Override: CYCLE_TIMING=0. Musi byc PRZED Emulator::new() (Machine czyta env w konstruktorze).
    if std::env::var_os("CYCLE_TIMING").is_none() {
        std::env::set_var("CYCLE_TIMING", "1");
    }
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
    // Regulacja tempa live: Z = wolniej (+wait-states flasha), X = szybciej (-). Edge-detect.
    let mut zx_was = (false, false);
    // KEYLOG=1: loguj krawedzie fizycznych klawiszy (Twoje wcisniecia) do korelacji z firmware.
    let key_log = std::env::var("KEYLOG").is_ok();
    let mut klog_prev = [false; 16];
    // Klawiatura: JEDEN krotki impuls na fizyczne nacisniecie (bez auto-repeatu). Firmware
    // repetuje trzymany klawisz - wiec na krawedzi narastajacej dajemy puls ~PULSE_CYCLES
    // (pod-klatkowo, dzielony run_steps) i potem klawisz pozostaje PUSZCZONY mimo trzymania.

    // Audio buzzera (cpal). None = brak urzadzenia/formatu -> gra bez dzwieku. NO_AUDIO=1 wylacza.
    let audio = if std::env::var("NO_AUDIO").is_ok() {
        None
    } else {
        match audio::Buzzer::new() {
            Some(a) => { println!("[audio] buzzer aktywny"); Some(a) }
            None => { eprintln!("[audio] brak wyjscia audio - gra bez dzwieku"); None }
        }
    };

    // Pacing do prawdziwego zegara 13 MHz: budzet krokow rosnie z czasem sciany,
    // z limitem (nie nadrabiamy wiecej niz 50 ms naraz - brak spirali smierci).
    let mut last = Instant::now();
    let mut budget = 0.0f64;
    let max_batch = CLOCK_HZ * 0.05;
    // Adaptacyjne CPI: ile instrukcji wykonac na klatke = budzet_cykli / cpi. Aktualizowane
    // z RZECZYWISTEGO CPI ostatniej klatki, wiec sledzi zmiany WS (Z/X) bez zrywania pacingu.
    let mut cpi_est = CPI_EST;
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
                // Budzet w CYKLACH CPU (13 MHz). Instrukcja kosztuje ~CPI cykli (wait-states
                // flasha), wiec liczymy ile instrukcji wykonac z estymaty CPI, a PO wykonaniu
                // odejmujemy rzeczywiscie zuzyte cykle (samokorekta). CPU realnie @13 MHz -
                // delaye/animacje/gry chodza w realnym tempie, nie ~3.3x za szybko.
                budget += dt * CLOCK_HZ;
                if budget > max_batch {
                    budget = max_batch; // nie kumuluj dlugu
                }
                let n = (budget / cpi_est).max(0.0) as u64;
                let cyc0 = e.total_cycles();
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
                // Stan klawiszy telefonu = stan fizyczny klawiszy PC (proste trzymanie).
                for (i, (ek, phys)) in logical.iter().enumerate() {
                    if key_log && *phys != klog_prev[i] {
                        eprintln!("[gui_key] {} {:?} cyc={}", if *phys { "DOWN" } else { "UP  " }, ek, e.total_cycles());
                        klog_prev[i] = *phys;
                    }
                    e.set_key(*ek, *phys);
                }
                e.run_steps(n);
                // Samokorekta budzetu: odejmij RZECZYWIScie zuzyte cykle (nie estymate).
                let consumed = e.total_cycles() - cyc0;
                budget -= consumed as f64;
                // Audio: zaktualizuj buzzer (dzwonki, PUP) + ton DSP (DTMF klawiszy, glosnik rozmow).
                if let Some(a) = &audio {
                    let (bf, bv, bp) = e.buzzer_state();
                    a.update(bf, bv, bp);
                    let (t1, t2, tp) = e.dsp_tone_state();
                    a.update_tone(t1, t2, tp);
                }
                // Aktualizuj estymate CPI z faktycznego zuzycia (sledzi WS zmieniane Z/X).
                if n > 0 {
                    cpi_est = (consumed as f64 / n as f64).clamp(1.0, 80.0);
                }
                if frame % 30 == 0 && std::env::var("GUI_DBG").is_ok() {
                    eprintln!("[gui] pc={:#08X} steps={} cyc={} dt={:.1}ms n={} effMHz={:.1} lit={}",
                        e.pc(), e.total_steps, e.total_cycles(), dt * 1000.0, n, eff_mhz, e.lcd_any_lit());
                }
                // Zatrzaskuj ostatnia NIEPUSTA klatke (tresc przetrwa transient/crash).
                if e.lcd_any_lit() {
                    for y in 0..HEIGHT {
                        for x in 0..WIDTH {
                            lcd.set(x as i32, y as i32, e.lcd_get(x, y));
                        }
                    }
                }
                // Regulacja tempa: Z = wolniej (+1 wait-state flasha), X = szybciej (-1).
                // Edge-detect (jeden krok na nacisniecie). Pokazuje w tytule + stderr.
                let z = window.is_key_down(Key::Z);
                let x = window.is_key_down(Key::X);
                if z && !zx_was.0 {
                    e.set_flash_ws(e.flash_ws() + 1);
                    eprintln!("[tempo] FLASH_WS={} (wolniej)", e.flash_ws());
                }
                if x && !zx_was.1 {
                    e.set_flash_ws(e.flash_ws().saturating_sub(1).max(1));
                    eprintln!("[tempo] FLASH_WS={} (szybciej)", e.flash_ws());
                }
                zx_was = (z, x);
                // Efektywna predkosc co ~1 s - mierzona w CYKLACH (powinno byc ~13 MHz).
                if spd_t.elapsed().as_secs_f64() >= 1.0 {
                    eff_mhz = (e.total_cycles() - spd_steps0) as f64
                        / spd_t.elapsed().as_secs_f64()
                        / 1.0e6;
                    spd_steps0 = e.total_cycles();
                    spd_t = Instant::now();
                }
                if frame % 8 == 0 {
                    let status = if e.crashed { "STOP" } else { "running" };
                    window.set_title(&format!(
                        "Nokia 3310 [{}] - {:.1} MHz (cykle) - WS={} [Z wolniej/X szybciej] - {}{}",
                        e.firmware_id,
                        eff_mhz,
                        e.flash_ws(),
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
        // Panel klawiatury: stan FIZYCZNY klawiszy (co user trzyma teraz) wg KEYPAD.
        let kd = |k: Key| window.is_key_down(k);
        let down = [
            kd(Key::Enter),                       // MENU
            kd(Key::Up),                          // UP
            kd(Key::Backspace),                   // C
            kd(Key::P),                           // PWR
            kd(Key::Down),                        // DOWN
            kd(Key::Key1) || kd(Key::NumPad1),    // 1
            kd(Key::Key2) || kd(Key::NumPad2),    // 2
            kd(Key::Key3) || kd(Key::NumPad3),    // 3
            kd(Key::Key4) || kd(Key::NumPad4),    // 4
            kd(Key::Key5) || kd(Key::NumPad5),    // 5
            kd(Key::Key6) || kd(Key::NumPad6),    // 6
            kd(Key::Key7) || kd(Key::NumPad7),    // 7
            kd(Key::Key8) || kd(Key::NumPad8),    // 8
            kd(Key::Key9) || kd(Key::NumPad9),    // 9
            kd(Key::LeftBracket),                 // *
            kd(Key::Key0) || kd(Key::NumPad0),    // 0
            kd(Key::RightBracket),                // #
        ];
        draw_keypad(&mut framebuf, &down);
        window
            .update_with_buffer(&framebuf, WIN_W, WIN_H)
            .expect("blad aktualizacji okna");
        frame += 1;
    }
}

/// Rysuje tekst (czcionka 5x7 z font.rs) wprost na bufor okna.
fn draw_text_fb(out: &mut [u32], px: usize, py: usize, text: &str, color: u32) {
    let mut cx = px;
    for ch in text.chars() {
        let g = font::glyph(ch.to_ascii_uppercase());
        for (col, &bits) in g.iter().enumerate() {
            for row in 0..font::H {
                if bits & (1 << row) != 0 {
                    let (x, y) = (cx + col, py + row);
                    if x < WIN_W && y < WIN_H {
                        out[y * WIN_W + x] = color;
                    }
                }
            }
        }
        cx += font::W + 1;
    }
}

/// Rysuje panel klawiatury pod ekranem: kazdy klawisz jako pudelko z etykieta,
/// wcisniety = jasny zielony. Pomaga zobaczyc co dociera do firmware (debug nawigacji).
fn draw_keypad(out: &mut [u32], down: &[bool; 17]) {
    let panel_y = MARGIN + HEIGHT * SCALE + 16;
    let (bw, bh, gap_x, gap_y) = (56usize, 18usize, 8usize, 4usize);
    let start_x = (WIN_W - (3 * bw + 2 * gap_x)) / 2;
    for (i, (label, col, row)) in KEYPAD.iter().enumerate() {
        let bx = start_x + col * (bw + gap_x);
        let by = panel_y + row * (bh + gap_y);
        let on = down[i];
        let box_col = if on { COL_KEY_ON } else { COL_KEY };
        for y in by..(by + bh) {
            for x in bx..(bx + bw) {
                if x < WIN_W && y < WIN_H {
                    out[y * WIN_W + x] = box_col;
                }
            }
        }
        let tw = label.chars().count() * (font::W + 1);
        let tx = bx + bw.saturating_sub(tw) / 2;
        let ty = by + (bh - font::H) / 2;
        let tcol = if on { 0x0000_2800 } else { 0x00A0_C0A0 };
        draw_text_fb(out, tx, ty, label, tcol);
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
