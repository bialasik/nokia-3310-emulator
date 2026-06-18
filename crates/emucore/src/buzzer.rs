//! Model buzzera PUP (MAD2/3310). Rejestry (baza IO 0x20000, z MADos hw/buzzer.c + MAME):
//!   IO_PUP_CTRL  = 0x20015, bit 0x20 = BUZ_EN (wlaczenie buzzera), bit 0x10 = VIB_EN (wibra).
//!   IO_PUP_BUZ_FH= 0x2001C / IO_PUP_BUZ_FL=0x2001D - dzielnik 16-bit = 13_000_000 / freq_Hz.
//!   IO_PUP_BUZ_V = 0x2001E - glosnosc 0x00..0xFF.
//! Firmware: buzzer_setfreq(f) zapisuje 13e6/f do FH/FL; buzzer_setvol(v) -> BUZ_V; init ustawia
//! BUZ_EN. Buzzer generuje fale prostokatna o czest. 13e6/dzielnik, amplituda ~ glosnosc.

const PUP_CTRL: u32 = 0x0002_0015;
const BUZ_FH: u32 = 0x0002_001C;
const BUZ_FL: u32 = 0x0002_001D;
const BUZ_V: u32 = 0x0002_001E;
const BUZ_EN_BIT: u8 = 0x20;

#[derive(Default)]
pub struct Buzzer {
    div: u16,    // dzielnik czestotliwosci (13e6/freq)
    vol: u8,     // glosnosc 0..255
    enabled: bool, // PUP_CTRL bit 0x20
    log: bool,   // BUZ_LOG: loguj WSZYSTKIE zapisy regionu PUP (diagnostyka dzwonka)
}

impl Buzzer {
    pub fn new() -> Self {
        Self {
            log: std::env::var("BUZ_LOG").is_ok(),
            ..Default::default()
        }
    }

    /// Obserwuj zapis MMIO (nie konsumuje - wartosc i tak ladowana do mmio, by odczyt dzialal).
    pub fn observe_write(&mut self, addr: u32, val: u8) {
        match addr {
            PUP_CTRL => self.enabled = val & BUZ_EN_BIT != 0,
            BUZ_FH => self.div = (self.div & 0x00FF) | ((val as u16) << 8),
            BUZ_FL => self.div = (self.div & 0xFF00) | val as u16,
            BUZ_V => self.vol = val,
            _ => {}
        }
        // BUZ_LOG: loguj WSZYSTKIE zapisy regionu PUP (0x20014..0x2001F) - zlapie tez ew. inna
        // sciezke audio dzwonka (PWM/wibra/sasiednie rejestry), nie tylko 4 znane rejestry buzzera.
        if self.log && (0x0002_0014..=0x0002_001F).contains(&addr) {
            let (f, v, p) = self.state();
            eprintln!("[buz] @{addr:#08X}={val:#04X} (freq={f}Hz vol={v} play={p})");
        }
    }

    /// Stan do syntezy audio: (czestotliwosc_Hz, glosnosc 0..255, czy_gra).
    /// freq = 13e6/dzielnik; gra gdy wlaczony, glosnosc>0 i dzielnik w sensownym zakresie.
    pub fn state(&self) -> (u32, u8, bool) {
        let freq = if self.div >= 2 { 13_000_000u32 / self.div as u32 } else { 0 };
        // Slyszalny zakres ~20 Hz..20 kHz. div=520 (freq 25 kHz) to nosna PCM - traktujemy jako brak tonu.
        let playing = self.enabled && self.vol > 0 && (20..=12_000).contains(&freq);
        (freq, self.vol, playing)
    }
}
