//! Minimalny model CTSI: kontroler przerwan + timer (DCT3/3310).
//!
//! Wg MADos hw/int.c / hw/timer.c (rejestry bajtowe, baza 0x20000, dostep BIG-ENDIAN):
//!   FIQL=0x20008 (latch FIQ), IRQL=0x20009, FIQM=0x2000A, IRQM=0x2000B, ICR=0x2000C,
//!   TMR0D=0x2000F, TMR0=0x20010/11, TMR0T(target)=0x20012/13, TMR1=0x20004/05.
//! - Timer = zrodlo FIQ. `timer_advance`: TMR0T = TMR0 + 0x200 (re-arm w ISR).
//! - Latch zwraca TYLKO realnie aktywne zrodla (inaczej handler dispatchuje null -> crash).
//! - Zapis do latcha kasuje wskazane bity (write-1-to-clear).

// Adresy absolutne rejestrow.
const FIQL: u32 = 0x0002_0008;
const IRQL: u32 = 0x0002_0009;
const FIQM: u32 = 0x0002_000A;
const IRQM: u32 = 0x0002_000B;
const ICR: u32 = 0x0002_000C;
const TMR0D: u32 = 0x0002_000F;
const TMR0_HI: u32 = 0x0002_0010;
const TMR0_LO: u32 = 0x0002_0011;
const TMR0T_HI: u32 = 0x0002_0012;
const TMR0T_LO: u32 = 0x0002_0013;
const TMR1_HI: u32 = 0x0002_0004;
const TMR1_LO: u32 = 0x0002_0005;
const PUP_FIQ8: u32 = 0x0002_0016; // timer = FIQ8: EN=0x01, ACT=0x02, MSK=0x04

pub struct Ctsi {
    pub fiq_latch: u8,
    pub irq_latch: u8,
    pub fiq_mask: u8, // bit=1 => zamaskowane
    pub irq_mask: u8,
    pub fiq_en: bool,
    pub irq_en: bool,

    tmr0: u16,
    tmr0_target: u16,
    tmr1: u16,
    timer_armed: bool,
    /// Interwal timera (target - tmr0 w chwili uzbrojenia). Do periodycznego re-armu
    /// targetu BEZ resetu TMR0 (free-running), czego wymaga OS software-timer (0x101/0x103).
    timer_period: u16,

    /// Periodyczny tick IRQ (systemowy) - one-shot, handler IRQ go nie kasuje (sam inkr. licznik).
    pub irq_tick_pending: bool,
    /// Rejestr PUP_FIQ8 (timer jako FIQ8): EN/ACT/MSK. ACT ustawiany przy wystrzale timera.
    pub pup_fiq8: u8,

    /// Numer zrodla FIQ timera (tunowalny przez env TIMER_FIQ_BIT).
    pub timer_fiq_bit: u8,
    /// Czy generowac systemowy tick IRQ (env TIMER_IRQ, domyslnie tak - stock; MADos=0).
    irq_tick_enabled: bool,
    /// Ile krokow CPU na 1 tick TMR0 (env TIMER_STEP_DIV).
    step_div: u32,
    step_ctr: u32,

    /// MBUSTIM (FIQ bit 3) - staly tick systemowy ~423.1 Hz stockowego firmware'u.
    /// Niezalezny od TMR0 (ktory napedza PTIMER=bit 4). 0 = wylaczony (env MBUSTIM_DIV).
    mbustim_div: u32,
    mbustim_ctr: u32,
    /// Auto-reload TMR0 po dopasowaniu (env TIMER_AUTORELOAD; periodyk dla v6.39).
    timer_autoreload: bool,

    pub fiq_fires: u64,
}

impl Default for Ctsi {
    fn default() -> Self {
        Self::new()
    }
}

impl Ctsi {
    pub fn new() -> Self {
        // Mapa FIQ (z MADos hw/int.h): bit 3 = MBUSTIM (staly tick ~423.1 Hz, glowny
        // timebase OS), bit 4 = PTIMER (programowalny, napedzany TMR0). TMR0-timer pali
        // domyslnie bit 4 (PTIMER); MBUSTIM modelujemy osobno (mbustim_div).
        let timer_fiq_bit = std::env::var("TIMER_FIQ_BIT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(4);
        let step_div = std::env::var("TIMER_STEP_DIV")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1024)
            .max(1);
        // MBUSTIM ~423.1 Hz @ 13 MHz => co ~30733 instrukcji. Domyslnie wlaczony (stock);
        // MADos uruchamiamy z MBUSTIM_DIV=0.
        let mbustim_div = std::env::var("MBUSTIM_DIV")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(30733);
        Self {
            fiq_latch: 0,
            irq_latch: 0,
            fiq_mask: 0,
            irq_mask: 0,
            fiq_en: false,
            irq_en: false,
            tmr0: 0,
            tmr0_target: 0,
            tmr1: 0,
            timer_armed: false,
            timer_period: 0,
            irq_tick_pending: false,
            pup_fiq8: 0,
            // Sztuczny systemowy tick IRQ jest NIEWIERNY: wstrzykuje IRQ bez ustawienia
            // IRQL, a handler IRQ stocka (0x2e5b40) czyta IRQL by poznac zrodlo -> przy
            // pustym IRQL dispatchuje smieci i rozjezdza kontekst/stos (crash skok do 0).
            // Domyslnie WYLACZONY; IRQ ma przychodzic z realnych zrodel. Env TIMER_IRQ=1 wlacza.
            irq_tick_enabled: std::env::var("TIMER_IRQ").map(|v| v == "1").unwrap_or(false),
            timer_fiq_bit,
            step_div,
            step_ctr: 0,
            mbustim_div,
            mbustim_ctr: 0,
            timer_autoreload: std::env::var("TIMER_AUTORELOAD").map(|v| v == "1").unwrap_or(false),
            fiq_fires: 0,
        }
    }

    /// Odczyt rejestru CTSI; None => nie nasz adres (Machine uzyje MMIO).
    pub fn read(&self, addr: u32) -> Option<u8> {
        Some(match addr {
            FIQL => self.fiq_latch,
            IRQL => self.irq_latch,
            FIQM => self.fiq_mask,
            IRQM => self.irq_mask,
            TMR0_HI => (self.tmr0 >> 8) as u8,
            TMR0_LO => self.tmr0 as u8,
            TMR0T_HI => (self.tmr0_target >> 8) as u8,
            TMR0T_LO => self.tmr0_target as u8,
            TMR1_HI => (self.tmr1 >> 8) as u8,
            TMR1_LO => self.tmr1 as u8,
            PUP_FIQ8 => self.pup_fiq8,
            // ICR odczyt: handlery FIQ/IRQ testuja bity "disabled" (FIQD=0x02, IRQD=0x08)
            // i jesli ustawione -> traktuja przerwanie jako spurious i wracaja bez kasowania.
            // Zwracamy stan: wlaczone => bity disabled CZYSTE.
            ICR => {
                (if self.fiq_en { 0x01 } else { 0x02 }) | (if self.irq_en { 0x04 } else { 0x08 })
            }
            _ => return None,
        })
    }

    /// Zapis rejestru CTSI; true => obsluzono.
    pub fn write(&mut self, addr: u32, val: u8) -> bool {
        match addr {
            FIQL => self.fiq_latch &= !val, // write-1-to-clear
            IRQL => self.irq_latch &= !val,
            FIQM => self.fiq_mask = val,
            IRQM => self.irq_mask = val,
            ICR => {
                if val & 0x01 != 0 {
                    self.fiq_en = true;
                }
                if val & 0x02 != 0 {
                    self.fiq_en = false;
                }
                if val & 0x04 != 0 {
                    self.irq_en = true;
                }
                if val & 0x08 != 0 {
                    self.irq_en = false;
                }
            }
            TMR0D => {} // dzielnik - pomijamy
            // Zapis licznika TMR0 (firmware moze go RESETOWAC, np. =0, przed uzbrojeniem
            // targetu): bez tego wolnobiezny tmr0 przelatuje staly target i FIQ nie pada.
            TMR0_HI => self.tmr0 = (self.tmr0 & 0x00FF) | ((val as u16) << 8),
            TMR0_LO => self.tmr0 = (self.tmr0 & 0xFF00) | val as u16,
            PUP_FIQ8 => self.pup_fiq8 = val, // firmware: EN/MSK + kasowanie ACT
            TMR0T_HI => self.tmr0_target = (self.tmr0_target & 0x00FF) | ((val as u16) << 8),
            TMR0T_LO => {
                self.tmr0_target = (self.tmr0_target & 0xFF00) | val as u16;
                self.timer_armed = true; // ustawienie targetu = uzbrojenie timera
                // Zapamietaj interwal do periodycznego re-armu (free-running TMR0).
                self.timer_period = self.tmr0_target.wrapping_sub(self.tmr0);
            }
            _ => return false,
        }
        true
    }

    /// Krok timera (wolany co krok CPU). Po elapsed ustawia bit FIQ timera w latchu.
    pub fn tick(&mut self) {
        // MBUSTIM (bit 3) - staly tick systemowy stockowego firmware'u (~423 Hz).
        if self.mbustim_div > 0 {
            self.mbustim_ctr += 1;
            if self.mbustim_ctr >= self.mbustim_div {
                self.mbustim_ctr = 0;
                self.fiq_latch |= 1 << 3;
            }
        }
        self.step_ctr += 1;
        if self.step_ctr >= self.step_div {
            self.step_ctr = 0;
            self.tmr0 = self.tmr0.wrapping_add(1);
            self.tmr1 = self.tmr1.wrapping_add(1);
            if self.timer_armed && self.tmr0 == self.tmr0_target {
                // Bit FIQL ustawiamy tylko gdy < 8 (stock firmware). Dla MADos timer to
                // wylacznie FIQ8 (PUP_FIQ8) - ustawienie zbednego bitu FIQL wywoluje
                // dispatch nieistniejacej rutyny i crash. TIMER_FIQ_BIT>=8 => pomijamy.
                if self.timer_fiq_bit < 8 {
                    // Zrodlo timera = CTSI FIQL bit (stock: 2, MADos: 4 -> task-switch fiq&0x10).
                    self.fiq_latch |= 1 << self.timer_fiq_bit;
                } else if self.pup_fiq8 & 0x01 != 0 {
                    // Tryb FIQ8 (PUP_FIQ8) - ustaw ACT jesli wlaczony.
                    self.pup_fiq8 |= 0x02;
                }
                if self.irq_tick_enabled {
                    self.irq_tick_pending = true; // systemowy tick IRQ (stock firmware)
                }
                // Periodyczny re-arm: zamiast resetowac TMR0 (co psuje free-running
                // licznik OS software-timera 0x101/0x103 -> SIM ATR timeout nie pali),
                // przesuwamy TARGET o interwal. TMR0 free-runuje (zawija na 0xffff jak
                // realny sprzet), FIQ pada periodycznie co `timer_period`. Env TIMER_AUTORELOAD=1.
                if self.timer_autoreload {
                    let p = if self.timer_period == 0 { 0x200 } else { self.timer_period };
                    self.tmr0_target = self.tmr0_target.wrapping_add(p);
                }
            }
        }
    }

    /// Czy nalezy asertowac FIQ (jest aktywne, niezamaskowane zrodlo i FIQ wlaczony).
    pub fn fiq_active(&self) -> bool {
        if !self.fiq_en {
            return false;
        }
        let ctsi = (self.fiq_latch & !self.fiq_mask) != 0;
        // FIQ8 (PUP_FIQ8): aktywny gdy EN(0x01) i ACT(0x02) i nie-MSK(0x04). Timer MADos.
        let pup8 = self.pup_fiq8 & 0x01 != 0 && self.pup_fiq8 & 0x02 != 0 && self.pup_fiq8 & 0x04 == 0;
        ctsi || pup8
    }

    pub fn irq_active(&self) -> bool {
        self.irq_en && (self.irq_latch & !self.irq_mask) != 0
    }

    /// Diagnostyka: (fiq_en, fiq_mask, fiq_latch, irq_mask, tmr0, tmr0_target, timer_armed).
    pub fn debug_state(&self) -> (bool, u8, u8, u8, u16, u16, bool) {
        (self.fiq_en, self.fiq_mask, self.fiq_latch, self.irq_mask,
         self.tmr0, self.tmr0_target, self.timer_armed)
    }

    /// Pobiera i kasuje oczekujacy systemowy tick IRQ (one-shot), o ile IRQ wlaczone.
    pub fn take_irq_tick(&mut self) -> bool {
        if self.irq_en && self.irq_tick_pending {
            self.irq_tick_pending = false;
            true
        } else {
            false
        }
    }
}
