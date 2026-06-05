//! Enkapsulacja emulatora dla frontendu: CPU ARM7TDMI + magistrala (Machine)
//! + timer/przerwania. Frontend tylko tworzy `Emulator`, napedza `run_steps`
//! i czyta bufor LCD oraz wstrzykuje klawisze.

use crate::lcd;
use crate::machine::FW_ENTRY;
use crate::{loader, Machine};
use arm7tdmi::Arm7tdmiCore;
use rustboyadvance_utils::Shared;
use std::panic::AssertUnwindSafe;

pub const LCD_W: usize = lcd::WIDTH;
pub const LCD_H: usize = lcd::HEIGHT;

pub struct Emulator {
    cpu: Arm7tdmiCore<Machine>,
    pub firmware_id: String,
    pub total_steps: u64,
    pub crashed: bool,
    /// Eksperymentalny mock bariery stanu bootu stocka (env FORCE_R5).
    force_r5: bool,
    /// Wymus powod wlaczenia r4 na bramce (env FORCE_REASON).
    force_reason: Option<u32>,
    /// Wymus przejscie 7 warunkow glownej petli bootu (env FORCE_BOOT).
    force_boot: bool,
}

impl Emulator {
    /// Laduje ROM, buduje maszyne i ustawia CPU na punkt wejscia firmware.
    pub fn new() -> Result<Self, String> {
        let rom = loader::load()?.ok_or("brak plikow ROM w roms/ ani crates/rom/")?;
        let firmware_id = rom.firmware_id.clone();
        let machine = Machine::new(rom.mem);
        let bus = Shared::new(machine);
        let mut cpu = Arm7tdmiCore::new(bus);
        cpu.reset();
        cpu.set_reg(15, FW_ENTRY);
        cpu.reload_pipeline32();
        Ok(Self {
            cpu,
            firmware_id,
            total_steps: 0,
            crashed: false,
            force_r5: std::env::var("FORCE_R5").is_ok(),
            force_reason: std::env::var("FORCE_REASON").ok().and_then(|s| s.parse().ok()),
            force_boot: std::env::var("FORCE_BOOT").is_ok(),
        })
    }

    /// Wykonuje `n` krokow CPU (tick timera + asercja FIQ/IRQ co krok).
    /// Panike rdzenia (np. niezdekodowana instrukcja) lapiemy, by nie ubic okna.
    pub fn run_steps(&mut self, n: u64) {
        if self.crashed {
            return;
        }
        let cpu = &mut self.cpu;
        let force_r5 = self.force_r5;
        let force_reason = self.force_reason;
        let force_boot = self.force_boot;
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            for _ in 0..n {
                let pc = cpu.get_next_pc();
                // EKSPERYMENT (env FORCE_R5): mock komunikatu "stan bootu=1" w barierze
                // 0x2ef9b6 stocka + wcisniecie POWER (skan zwraca 0x81). Pozwala przejsc
                // do renderu ekranu startowego. Domyslnie wylaczone.
                if force_r5 && pc == 0x002E_F9B6 {
                    cpu.set_reg(5, 1);
                    cpu.bus.keypad.set_power(true);
                }
                // POWER zwolnij gdy firmware wejdzie w kpd_wait_release (0x338d68, czeka
                // KPD_C=0x7F=nic wcisniete) - inaczej trzymany POWER blokuje wait_release.
                if force_r5 && pc == 0x0033_8D68 {
                    cpu.bus.keypad.set_power(false);
                }
                // FORCE_REASON: powod wlaczenia (r4) na bramce 0x2ef924 (0x6C po logo -> 2,
                // by firmware nie resetowal sie po fazie powitalnej).
                if let Some(v) = force_reason {
                    if pc == 0x002E_F924 { cpu.set_reg(4, v); }
                }
                // FORCE_BOOT: wymus r0=1 po 7 funkcjach warunku glownej petli bootu
                // (0x2e1a76..0x2e1aa6) - przejscie do glownej petli zdarzen.
                if force_boot && matches!(pc,
                    0x002E_1A76 | 0x002E_1A7E | 0x002E_1A86 | 0x002E_1A8E
                    | 0x002E_1A96 | 0x002E_1A9E | 0x002E_1AA6) {
                    cpu.set_reg(0, 1);
                }
                cpu.bus.pc_hint = pc;
                cpu.step();
                // Programowy reset CPU (firmware zapisal bit2 do IO_CTSI_RST):
                // restart od FW_ENTRY. RAM zachowany (to reset rdzenia, nie pamieci;
                // crt0 sam czysci bss). Za drugim startem powod wlaczenia jest inny.
                if cpu.bus.reset_request {
                    cpu.bus.reset_request = false;
                    cpu.bus.reset_count += 1;
                    cpu.reset();
                    cpu.set_reg(15, FW_ENTRY);
                    cpu.reload_pipeline32();
                    continue;
                }
                cpu.bus.tick_timer();
                if cpu.bus.fiq_active() {
                    cpu.fiq();
                } else if cpu.bus.irq_active() || cpu.bus.take_irq_tick() {
                    cpu.irq();
                }
            }
        }));
        if result.is_err() {
            self.crashed = true;
            eprintln!("[emu] rdzen zatrzymany (panika) po {} krokach", self.total_steps);
        }
        self.total_steps += n;
    }

    /// Stan piksela LCD (z dekodera PCD8544).
    #[inline]
    pub fn lcd_get(&self, x: usize, y: usize) -> bool {
        self.cpu.bus.lcd.get(x, y)
    }

    /// Czy ekran cokolwiek pokazuje.
    pub fn lcd_any_lit(&self) -> bool {
        self.cpu.bus.lcd.any_lit()
    }

    /// Aktualny PC (diagnostyka).
    pub fn pc(&self) -> u32 {
        self.cpu.get_next_pc()
    }

    /// Liczba zapisow danych do LCD (diagnostyka postepu renderu).
    pub fn lcd_data_writes(&self) -> u64 {
        self.cpu.bus.lcd.data_writes
    }
    /// Diagnostyka CTSI: (fiq_en, fiq_mask, fiq_latch, irq_mask, tmr0, tmr0_target, armed).
    pub fn ctsi_state(&self) -> (bool, u8, u8, u8, u16, u16, bool) {
        self.cpu.bus.ctsi.debug_state()
    }
    /// Diagnostyka klawiatury: (odczyty KPD_C, z wykrytym klawiszem, asercje IRQ bit0).
    pub fn key_diag(&self) -> (u64, u64, u64) {
        (
            self.cpu.bus.keypad.read_c_count.get(),
            self.cpu.bus.keypad.read_c_hit.get(),
            self.cpu.bus.key_irq_asserts,
        )
    }
    /// Napiecie baterii w mV (z modelu CCONT).
    pub fn battery_mv(&self) -> u32 {
        self.cpu.bus.ccont.battery_mv()
    }
    /// Zegar RTC (godz, min, sek) z CCONT.
    pub fn clock(&self) -> (u8, u8, u8) {
        self.cpu.bus.ccont.clock()
    }
    /// Stan peryferiow wyjsciowych do okna: (LED, wibracja, brzeczyk_on, dzielnik_tonu).
    pub fn indicators(&self) -> (bool, bool, bool, u16) {
        let p = &self.cpu.bus.periph;
        (p.led_on(), p.vibra_on(), p.buzzer_on(), p.buzzer_div())
    }

    /// Wcisniecie/zwolnienie klawisza (mapowanie na matryce klawiatury 3310).
    pub fn set_key(&mut self, key: EmuKey, down: bool) {
        use crate::keypad::{KEY_CANCEL, KEY_DOWN, KEY_MENU, KEY_UP};
        // POWER ma osobna sciezke (zwiera bit kolumny 1 -> kod skanu 0x81).
        if let EmuKey::Power = key {
            self.cpu.bus.keypad.set_power(down);
            return;
        }
        let code = match key {
            EmuKey::Up => KEY_UP,
            EmuKey::Down => KEY_DOWN,
            EmuKey::Select => KEY_MENU,
            EmuKey::Back => KEY_CANCEL,
            // Firmware keycody v6.39: cyfry 1-9 = 0x01-0x09, 0 = 0x0a, * = 0x0b, # = 0x0c.
            EmuKey::Digit(0) => 0x0a,
            EmuKey::Digit(n) => n,
            EmuKey::Star => 0x0b,
            EmuKey::Hash => 0x0c,
            EmuKey::Power => unreachable!(),
        };
        if down {
            self.cpu.bus.keypad.press_code(code);
        } else {
            self.cpu.bus.keypad.release_code(code);
        }
    }
}

/// Klawisze telefonu (mapowane przez frontend z klawiatury PC).
#[derive(Clone, Copy)]
pub enum EmuKey {
    Up,
    Down,
    Select,
    Back,
    Power,
    /// Cyfra 0-9 (kod matrycy = cyfra). Do PIN, wybierania, skrotow menu.
    Digit(u8),
    Star,
    Hash,
}
