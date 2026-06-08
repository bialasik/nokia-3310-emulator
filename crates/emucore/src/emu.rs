//! Enkapsulacja emulatora dla frontendu: CPU ARM7TDMI + magistrala (Machine)
//! + timer/przerwania. Frontend tylko tworzy `Emulator`, napedza `run_steps`
//! i czyta bufor LCD oraz wstrzykuje klawisze.

use crate::lcd;
use crate::machine::FW_ENTRY;
use crate::{loader, Machine};
use arm7tdmi::Arm7tdmiCore;
use arm7tdmi::memory::DebugRead;
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
    /// Wymus akceptacje SIM (env SIM_ACCEPT): handler 0x2df51c dostaje 0x5E2 (offset 0x128 =
    /// SIM-REJECT). Przepisz na 0x5E1 (offset 0x127 = ACCEPT) -> pelna kaskada (post 0x32c +
    /// SIM-ready 0x2726be). Test czy odblokowuje standby/menu (vs samo wymuszenie flagi=limbo).
    sim_accept: bool,
    /// Histogram PC (bucket 0x100) w oknie krokow PCWIN - lokalizacja funkcji ewaluacji.
    pub pcwin_hist: std::collections::HashMap<u32, u64>,
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
            sim_accept: std::env::var("SIM_ACCEPT").is_ok(),
            pcwin_hist: std::collections::HashMap::new(),
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
        let sim_accept = self.sim_accept;
        // EMU_BP=hex[,hex...]: loguj trafienia PC (krok,r0,lr) - diagnostyka sciezki na wiernej
        // sciezce Emulatora (wintest/GUI), bo trace.rs nie bootuje wiernie (SIM nieaktywny).
        let emu_bps: Vec<u32> = std::env::var("EMU_BP").ok().map(|s|
            s.split(',').filter_map(|x| u32::from_str_radix(x.trim().trim_start_matches("0x"), 16).ok()).collect()
        ).unwrap_or_default();
        // EMU_BP_R0=hex: filtruj EMU_BP tylko gdy r0==val (np. konkretny msg id do 0x2e9896).
        let emu_bp_r0: Option<u32> = std::env::var("EMU_BP_R0").ok().and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok());
        let mut emu_bp_cnt = 0u32;
        let mut step_no = self.total_steps;
        // PCWIN="lo:hi": histogram PC (bucket 0x100) w oknie krokow [lo,hi] - lokalizuje
        // funkcje ewaluacji (np. decyzja accept/reject po init SIM). Wynik w self.pcwin_hist.
        let pcwin: Option<(u64, u64)> = std::env::var("PCWIN").ok().and_then(|s| {
            let mut it = s.splitn(2, ':');
            Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
        });
        let pcwin_hist = &mut self.pcwin_hist;
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            for _ in 0..n {
                let pc = cpu.get_next_pc();
                if let Some((lo, hi)) = pcwin {
                    if step_no >= lo && step_no <= hi && (0x0020_0000..0x0034_0000).contains(&pc) {
                        *pcwin_hist.entry(pc & !0xFF).or_insert(0) += 1;
                    }
                }
                if !emu_bps.is_empty() && emu_bps.contains(&pc) && emu_bp_cnt < 60
                    && emu_bp_r0.map(|v| cpu.get_reg(0) == v).unwrap_or(true) {
                    emu_bp_cnt += 1;
                    let mem = std::env::var("EMU_BP_MEM").ok().and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok());
                    let memstr = mem.map(|a| {
                        let b = [cpu.bus.debug_read_8(a), cpu.bus.debug_read_8(a+1), cpu.bus.debug_read_8(a+2), cpu.bus.debug_read_8(a+3)];
                        format!(" mem[{a:#08X}]={:02X} {:02X} {:02X} {:02X}", b[0], b[1], b[2], b[3])
                    }).unwrap_or_default();
                    eprintln!("[EMU_BP @{:#08X} #{emu_bp_cnt} krok {step_no}] r0={:08X} r1={:08X} r2={:08X} lr={:08X}{memstr}",
                        pc, cpu.get_reg(0), cpu.get_reg(1), cpu.get_reg(2), cpu.get_reg(14));
                }
                step_no += 1;
                // SIM_ACCEPT: przepisz reject (0x5E2) na accept (0x5E1) na wejsciu handlera SIM.
                if sim_accept && pc == 0x002D_F51C && cpu.get_reg(0) == 0x5E2 {
                    cpu.set_reg(0, 0x5E1);
                }
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
