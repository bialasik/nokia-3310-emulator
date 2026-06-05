//! Peryferia wyjsciowe DCT3/3310: brzeczyk, wibracja, LED, watchdog — wg MADos
//! hw/buzzer.c, vibra.c, led.c, ccont.c (WDT). Sledzimy stan (obserwacyjnie), zapisy
//! i tak ladują się do mmio[] (odczyty zwracaja zapisane wartosci). Stan wystawiamy
//! getterami dla okna (wskazniki/audio) i diagnostyki.
//!
//! Rejestry PUP/UIF (baza 0x20000):
//!  PUP_CTRL=0x15 (VIB_EN=0x10, BUZ_EN=0x20), PUP_VIB=0x1B (freq b0-4, mode b5-6),
//!  PUP_BUZ_FH=0x1C, PUP_BUZ_FL=0x1D, PUP_BUZ_V=0x1E (glosnosc),
//!  PUP_GENIO=0x20, IO_UIF_CTRL3=0x33 (LED=0x02), IO_CTSI_WDT=0x03.

const REG_PUP_CTRL: u32 = 0x0002_0015;
const REG_PUP_VIB: u32 = 0x0002_001B;
const REG_PUP_BUZ_FH: u32 = 0x0002_001C;
const REG_PUP_BUZ_FL: u32 = 0x0002_001D;
const REG_PUP_BUZ_V: u32 = 0x0002_001E;
const REG_UIF_CTRL3: u32 = 0x0002_0033;
const REG_CTSI_WDT: u32 = 0x0002_0003;

#[derive(Default)]
pub struct Periph {
    pup_ctrl: u8,
    /// Brzeczyk: dzielnik czestotliwosci (FH<<8|FL) i glosnosc.
    buz_fh: u8,
    buz_fl: u8,
    pub buz_volume: u8,
    /// Wibracja: poziom/mode (PUP_VIB).
    pub vib: u8,
    /// LED (UIF_CTRL3 bit1).
    uif_ctrl3: u8,
    /// Watchdog: ostatnia wartosc karmienia (0x31=kick); licznik karmien.
    pub wdt_last: u8,
    pub wdt_kicks: u64,
}

impl Periph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Obserwuje zapis do rejestru peryferium wyjsciowego (nie konsumuje — mmio[] i tak
    /// zapisuje, by odczyty zwracaly wartosc). Aktualizuje stan dla okna/diagnostyki.
    pub fn observe_write(&mut self, addr: u32, val: u8) {
        match addr {
            REG_PUP_CTRL => self.pup_ctrl = val,
            REG_PUP_VIB => self.vib = val,
            REG_PUP_BUZ_FH => self.buz_fh = val,
            REG_PUP_BUZ_FL => self.buz_fl = val,
            REG_PUP_BUZ_V => self.buz_volume = val,
            REG_UIF_CTRL3 => self.uif_ctrl3 = val,
            REG_CTSI_WDT => {
                self.wdt_last = val;
                self.wdt_kicks += 1;
            }
            _ => {}
        }
    }

    /// Czy brzeczyk wlaczony (PUP_CTRL bit5) i z niezerowa glosnoscia.
    pub fn buzzer_on(&self) -> bool {
        self.pup_ctrl & 0x20 != 0 && self.buz_volume != 0
    }
    /// Dzielnik czestotliwosci brzeczyka (do wyliczenia tonu).
    pub fn buzzer_div(&self) -> u16 {
        ((self.buz_fh as u16) << 8) | self.buz_fl as u16
    }
    /// Czy wibracja wlaczona (PUP_CTRL bit4).
    pub fn vibra_on(&self) -> bool {
        self.pup_ctrl & 0x10 != 0
    }
    /// Czy dioda LED wlaczona (UIF_CTRL3 bit1).
    pub fn led_on(&self) -> bool {
        self.uif_ctrl3 & 0x02 != 0
    }
}
