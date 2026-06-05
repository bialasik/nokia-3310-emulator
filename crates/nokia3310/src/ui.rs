//! Tymczasowa warstwa wizualna (placeholder) na etap M0.
//! Pokazuje ekran startowy i przykladowe menu Series 20 sterowane klawiatura.
//! UWAGA: to atrapa demonstrujaca pipeline (czcionka + ekran + wejscie).
//! Zostanie usunieta, gdy firmware z ROM-u przejmie rysowanie ekranu (M3/M4).

use crate::lcd::{Lcd, WIDTH};
use std::time::{Duration, Instant};

/// Logiczne klawisze Nokii (mapowane z klawiatury PC we frontendzie).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Select, // srodkowy / lewy klawisz funkcyjny
    Back,   // 'C' / prawy klawisz funkcyjny
}

enum Screen {
    Splash { since: Instant },
    Menu,
}

/// Pozycje menu glownego Nokii 3310 (Series 20).
const MENU: &[&str] = &[
    "MESSAGES",
    "CONTACTS",
    "CALL LOG",
    "PROFILES",
    "SETTINGS",
    "GAMES",
    "SNAKE II",
];

pub struct Ui {
    screen: Screen,
    sel: usize,
    last_key: Option<&'static str>,
    rom_info: Option<String>,
}

impl Ui {
    /// `rom_info` = wykryta wersja firmware (lub None w trybie demo).
    pub fn new(rom_info: Option<String>) -> Self {
        Self {
            screen: Screen::Splash {
                since: Instant::now(),
            },
            sel: 0,
            last_key: None,
            rom_info,
        }
    }

    /// Reakcja na logiczny klawisz.
    pub fn on_key(&mut self, key: Key) {
        match self.screen {
            Screen::Splash { .. } => {
                // dowolny klawisz konczy splash
                self.screen = Screen::Menu;
            }
            Screen::Menu => match key {
                Key::Up => {
                    self.sel = (self.sel + MENU.len() - 1) % MENU.len();
                    self.last_key = Some("UP");
                }
                Key::Down => {
                    self.sel = (self.sel + 1) % MENU.len();
                    self.last_key = Some("DOWN");
                }
                Key::Select => self.last_key = Some("SELECT"),
                Key::Back => self.last_key = Some("BACK"),
            },
        }
    }

    /// Aktualizacja stanu zaleznego od czasu (auto-przejscie ze splash).
    pub fn tick(&mut self) {
        if let Screen::Splash { since } = self.screen {
            if since.elapsed() >= Duration::from_millis(2500) {
                self.screen = Screen::Menu;
            }
        }
    }

    /// Renderuje aktualny ekran do bufora LCD.
    pub fn render(&self, lcd: &mut Lcd) {
        lcd.clear();
        match self.screen {
            Screen::Splash { .. } => self.render_splash(lcd),
            Screen::Menu => self.render_menu(lcd),
        }
    }

    fn render_splash(&self, lcd: &mut Lcd) {
        // Wordmark "NOKIA" (skala 2).
        lcd.draw_text_centered(6, "NOKIA", 2, true);
        match &self.rom_info {
            Some(id) => {
                // Tokeny wersji firmware, kazdy w osobnej wysrodkowanej linii.
                let mut y = 24;
                for tok in id.split_whitespace().take(3) {
                    lcd.draw_text_centered(y, tok, 1, true);
                    y += 8;
                }
            }
            None => {
                lcd.draw_text_centered(26, "3310", 1, true);
                lcd.draw_text_centered(38, "DEMO", 1, true);
            }
        }
    }

    fn render_menu(&self, lcd: &mut Lcd) {
        // Pasek tytulu (inwersja).
        lcd.fill_rect(0, 0, WIDTH as i32, 9, true);
        lcd.draw_text(2, 1, "MENU", 1, false);

        // Lista 3 widocznych pozycji z paskiem przewijania kursora.
        let visible = 3;
        let top = self.sel.saturating_sub(visible - 1).min(MENU.len() - visible);
        for row in 0..visible {
            let idx = top + row;
            if idx >= MENU.len() {
                break;
            }
            let y = 11 + row as i32 * 11;
            let selected = idx == self.sel;
            if selected {
                lcd.fill_rect(0, y - 1, WIDTH as i32, 10, true);
            }
            lcd.draw_text(3, y, MENU[idx], 1, !selected);
        }

        // Strzalki przewijania.
        if top > 0 {
            lcd.draw_text(WIDTH as i32 - 7, 11, "<", 1, true); // placeholder gora
        }
    }
}
