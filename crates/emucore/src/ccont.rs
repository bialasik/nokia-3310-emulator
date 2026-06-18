//! Model CCONT (uklad zarzadzania zasilaniem/zegar/ADC) przez GENSIO.
//!
//! Wierny wg MADos hw/ccont.c (patrz pamiec: mados-hw-reference):
//!   write(reg,byte): CC_WR <- (reg<<3);  CC_WR <- byte
//!   read(reg):       CC_WR <- (reg<<3)|0x04;  r = CC_RD
//! Rejestry CC_WR=0x2002C, CC_RD=0x2006C (GENSIO).
//!
//! Kluczowe (z ccont.c):
//!  - ccont_test: `ccont_read(0x03) & 0xFC == 0xB0` -> reg3 ODCZYT = ID chipu 0xB0 | (AD hi).
//!  - ADC 10-bit multipleksowany: reg0 bity 4-6 = WYBOR ZRODLA; odczyt reg2 = AD lo,
//!    reg3 = 0xB0 | (AD hi & 0x03). Firmware ustawia zrodlo (reg0) i czyta reg2/reg3.
//!    Kanaly (MADos): 2=Vbat(bateria), 3=BSI(typ baterii), 5=Vchar(ladowarka), 7=Ichar.
//!    BLAD wczesniej: zwracalismy baterie dla KAZDEGO kanalu -> kanal ladowarki "widzial"
//!    napiecie -> firmware mogl wejsc w tryb ladowania. Teraz: ladowarka/prad = 0.
//!  - RTC: reg7=sec,8=min,9=hr,0A=day; wakeup reg0B=min,0C=hr.
//!  - reg0E = zrodlo przerwania CCONT; reg0F = enable (0=on).

pub const REG_CC_WR: u32 = 0x0002_002C;
pub const REG_CC_RD: u32 = 0x0002_006C;

/// Wartosci ADC (10-bit) per zrodlo, dobrane na zdrowy telefon bez ladowarki.
/// Vbat: (adval+22)*25/4 mV. 3.7V -> adval=570 (0x23A).
// UWAGA: wczesniej WSZYSTKIE kanaly zwracaly 570 (0x23A) i self-test przechodzil.
// Zachowujemy 570 dla baterii/temperatury (brak regresji), zerujemy TYLKO ladowarke.
const AD_VBAT: u16 = 570; // kanal 2: ~3.7V zdrowa bateria
const AD_BSI: u16 = 570; // kanal 3: wskaznik baterii (jak wczesniej - self-test OK)
const AD_VCHAR: u16 = 0; // kanal 5: brak ladowarki (BYLO 570 = falszywa ladowarka!)
const AD_ICHAR: u16 = 0; // kanal 7: brak pradu ladowania
const AD_BTEMP: u16 = 570; // kanaly inne: jak wczesniej (brak regresji)
const AD_RSSI: u16 = 0x3FF; // kanal 1: sila sygnalu odbieranego (wg MAME nokia_3310.cpp = pelny)
const AD_BSITYPE: u16 = 0x280; // kanal 3: typ baterii (wg MAME)

pub struct Ccont {
    regs: [u8; 0x10],
    reg_sel: usize,
    expect_data: bool,
    /// Aktualnie wybrane zrodlo ADC (reg0 bity 4-6).
    ad_source: u8,
    /// Akumulator krokow CPU dla tika RTC (1 sekunda = ~13e6 krokow @13MHz).
    rtc_steps: u64,
    /// Histogram odczytow/zapisow rejestrow (diagnostyka).
    pub read_count: [u32; 0x10],
    pub write_count: [u32; 0x10],
}

impl Default for Ccont {
    fn default() -> Self {
        Self::new()
    }
}

impl Ccont {
    pub fn new() -> Self {
        let mut regs = [0u8; 0x10];
        // Wartosci poczatkowe z ccont_init.
        regs[0x03] = 0xB0; // ID chipu (gorne 6 bitow), dolne 2 = AD hi (nadpisywane przy odczycie)
        regs[0x05] = 0x20;
        regs[0x06] = 0x54;
        // RTC: prawdopodobny czas startowy 12:00:00, dzien 1.
        regs[0x07] = 0; // sec
        regs[0x08] = 0; // min
        regs[0x09] = 12; // hr
        regs[0x0A] = 1; // day
        Self {
            regs,
            reg_sel: 0,
            expect_data: false,
            ad_source: 0,
            rtc_steps: 0,
            read_count: [0; 0x10],
            write_count: [0; 0x10],
        }
    }

    /// Tik RTC: wolany co krok CPU. Po ~13e6 krokach (1 s @13MHz) inkrementuje sekundy
    /// z przeniesieniem min/godz/dzien (reg7/8/9/0A). Daje "zywy" zegar w standby.
    pub fn tick(&mut self, cycles: u32) {
        self.rtc_steps += cycles as u64;
        if self.rtc_steps < 13_000_000 {
            return;
        }
        self.rtc_steps -= 13_000_000;
        let sec = (self.regs[0x07] & 0x3F) + 1;
        if sec < 60 {
            self.regs[0x07] = sec;
            return;
        }
        self.regs[0x07] = 0;
        let min = (self.regs[0x08] & 0x3F) + 1;
        if min < 60 {
            self.regs[0x08] = min;
            return;
        }
        self.regs[0x08] = 0;
        let hr = (self.regs[0x09] & 0x1F) + 1;
        if hr < 24 {
            self.regs[0x09] = hr;
            return;
        }
        self.regs[0x09] = 0;
        self.regs[0x0A] = (self.regs[0x0A] & 0x3F).wrapping_add(1);
    }

    /// Napiecie baterii w mV (z kanalu Vbat): (adval+22)*25/4 (wg ccont_get_ub).
    pub fn battery_mv(&self) -> u32 {
        ((AD_VBAT as u32 + 22) * 25) / 4
    }

    /// Aktualny czas RTC (godz, min, sek) z rejestrow 9/8/7.
    pub fn clock(&self) -> (u8, u8, u8) {
        (self.regs[0x09] & 0x1F, self.regs[0x08] & 0x3F, self.regs[0x07] & 0x3F)
    }

    /// Wartosc ADC (10-bit) dla aktualnie wybranego zrodla.
    fn ad_value(&self) -> u16 {
        match self.ad_source {
            1 => AD_RSSI,   // sila sygnalu (MAME: 0x3ff) - telefon czyta RSSI; wczesniej spadalo na BTEMP
            2 => AD_VBAT,
            3 => AD_BSITYPE, // typ baterii (MAME: 0x280)
            5 => AD_VCHAR,
            7 => AD_ICHAR,
            _ => AD_BTEMP,
        }
    }

    /// Zapis bajtu do GENSIO_CC_WR (maszyna stanow CCONT).
    pub fn cc_write(&mut self, byte: u8) {
        if self.expect_data {
            self.regs[self.reg_sel] = byte;
            self.write_count[self.reg_sel] += 1;
            // reg0: bity 4-6 wybieraja zrodlo ADC (ccont_set_adsrc).
            if self.reg_sel == 0x00 {
                self.ad_source = (byte >> 4) & 0x07;
            }
            self.expect_data = false;
        } else {
            self.reg_sel = ((byte >> 3) & 0x0F) as usize;
            let is_read = byte & 0x04 != 0;
            if !is_read {
                self.expect_data = true;
            }
        }
    }

    /// Odczyt z GENSIO_CC_RD = zawartosc wybranego rejestru.
    pub fn cc_read(&mut self) -> u8 {
        self.read_count[self.reg_sel] += 1;
        let ad = self.ad_value();
        match self.reg_sel {
            // reg2 = dolny bajt ADC wybranego zrodla.
            0x02 => (ad & 0xFF) as u8,
            // reg3 = ID chipu (0xB0) | gorne 2 bity ADC. ccont_test: &0xFC==0xB0.
            0x03 => 0xB0 | ((ad >> 8) & 0x03) as u8,
            // reszta: zawartosc rejestru (RTC/int/itd.).
            _ => self.regs[self.reg_sel],
        }
    }
}
