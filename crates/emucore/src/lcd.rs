//! Dekoder kontrolera LCD PCD8544 (84x48, mono) sterowanego przez GENSIO.
//!
//! Firmware DCT3 (zgodnie z MADos hw/lcd.c) wysyla:
//!   - bajt KOMENDY  -> zapis pod 0x0002006E (GENSIO_LCD_CMD)
//!   - bajt DANYCH   -> zapis pod 0x0002002E (GENSIO_LCD_DATA)
//! Kazdy bajt danych = 8 pionowych pikseli w aktualnym banku (kolumna), bit0 = gora.
//! Adresowanie poziome (V=0): po bajcie X++; przy X>83 X=0, Y(bank)++.

pub const WIDTH: usize = 84;
pub const HEIGHT: usize = 48;
pub const BANKS: usize = HEIGHT / 8; // 6

/// Adresy rejestrow GENSIO (baza 0x20000) dla danych/komend LCD.
pub const REG_LCD_DATA: u32 = 0x0002_002E;
pub const REG_LCD_CMD: u32 = 0x0002_006E;

pub struct Pcd8544 {
    /// Bufor pikseli: true = piksel zapalony.
    pix: [bool; WIDTH * HEIGHT],
    x: usize,        // 0..83
    bank: usize,     // 0..5 (Y)
    h: bool,         // rozszerzony zestaw instrukcji
    v: bool,         // adresowanie pionowe
    powered: bool,   // PD: false=on
    display_on: bool,
    inverse: bool,
    all_on: bool,
    /// licznik zapisanych bajtow danych (diagnostyka).
    pub data_writes: u64,
    pub cmd_writes: u64,
    pub nonzero_data: u64,
    pub cmd_log: Vec<u8>,
}

impl Default for Pcd8544 {
    fn default() -> Self {
        Self::new()
    }
}

impl Pcd8544 {
    pub fn new() -> Self {
        Self {
            pix: [false; WIDTH * HEIGHT],
            x: 0,
            bank: 0,
            h: false,
            v: false,
            powered: true,
            display_on: false,
            inverse: false,
            all_on: false,
            data_writes: 0,
            cmd_writes: 0,
            nonzero_data: 0,
            cmd_log: Vec::new(),
        }
    }

    /// Bajt komendy PCD8544.
    pub fn command(&mut self, b: u8) {
        self.cmd_writes += 1;
        if self.cmd_log.len() < 80 {
            self.cmd_log.push(b);
        }
        if b & 0xF8 == 0x20 {
            // Function set: 0010 0 PD V H
            self.powered = b & 0x04 == 0;
            self.v = b & 0x02 != 0;
            self.h = b & 0x01 != 0;
        } else if self.h {
            // Rozszerzony zestaw (kontrast/bias/temp) - bez wplywu na piksele.
        } else if b & 0xF8 == 0x08 {
            // Display control: 0000 1 D 0 E
            let d = b & 0x04 != 0;
            let e = b & 0x01 != 0;
            self.display_on = d || e;
            self.inverse = d && e; // 0x0D = inverse
            self.all_on = !d && e; // 0x09 = all-on
        } else if b & 0xC0 == 0x40 {
            // Set Y address (bank)
            self.bank = (b & 0x07) as usize % BANKS;
        } else if b & 0x80 == 0x80 {
            // Set X address
            self.x = (b & 0x7F) as usize % WIDTH;
        }
    }

    /// Bajt danych: kolumna 8 pikseli w aktualnym banku.
    pub fn data(&mut self, b: u8) {
        self.data_writes += 1;
        if b != 0 {
            self.nonzero_data += 1;
        }
        if self.x < WIDTH && self.bank < BANKS {
            for n in 0..8 {
                let y = self.bank * 8 + n;
                self.pix[y * WIDTH + self.x] = (b >> n) & 1 == 1;
            }
        }
        // auto-inkrement adresu
        if self.v {
            self.bank += 1;
            if self.bank >= BANKS {
                self.bank = 0;
                self.x = (self.x + 1) % WIDTH;
            }
        } else {
            self.x += 1;
            if self.x >= WIDTH {
                self.x = 0;
                self.bank = (self.bank + 1) % BANKS;
            }
        }
    }

    /// Efektywny stan piksela z uwzglednieniem trybu wyswietlania.
    #[inline]
    pub fn get(&self, x: usize, y: usize) -> bool {
        if x >= WIDTH || y >= HEIGHT || !self.display_on || !self.powered {
            return false;
        }
        if self.all_on {
            return true;
        }
        let p = self.pix[y * WIDTH + x];
        if self.inverse {
            !p
        } else {
            p
        }
    }

    /// Czy ekran cokolwiek pokazuje (do diagnostyki).
    pub fn any_lit(&self) -> bool {
        (0..HEIGHT).any(|y| (0..WIDTH).any(|x| self.get(x, y)))
    }

    /// Render ASCII (do debugowania w terminalu).
    pub fn to_ascii(&self) -> String {
        let mut s = String::with_capacity((WIDTH + 3) * (HEIGHT + 2));
        s.push('+');
        s.extend(std::iter::repeat('-').take(WIDTH));
        s.push_str("+\n");
        for y in 0..HEIGHT {
            s.push('|');
            for x in 0..WIDTH {
                s.push(if self.get(x, y) { '#' } else { ' ' });
            }
            s.push_str("|\n");
        }
        s.push('+');
        s.extend(std::iter::repeat('-').take(WIDTH));
        s.push('+');
        s
    }
}
