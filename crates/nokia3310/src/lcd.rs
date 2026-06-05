//! Bufor ekranu LCD 84x48 (1 bit/piksel), jak w Nokii 3310 (kontroler typu PCD8544).
//! W kolejnych etapach to wlasnie tutaj kontroler LCD bedzie wpisywal dane z firmware.

use crate::font;

/// Szerokosc ekranu w pikselach.
pub const WIDTH: usize = 84;
/// Wysokosc ekranu w pikselach.
pub const HEIGHT: usize = 48;

/// Monochromatyczny bufor ramki: true = piksel zapalony (ciemny).
pub struct Lcd {
    pix: [bool; WIDTH * HEIGHT],
}

impl Default for Lcd {
    fn default() -> Self {
        Self::new()
    }
}

impl Lcd {
    pub fn new() -> Self {
        Self {
            pix: [false; WIDTH * HEIGHT],
        }
    }

    /// Gasi caly ekran.
    pub fn clear(&mut self) {
        self.pix.iter_mut().for_each(|p| *p = false);
    }

    /// Stan piksela (z kontrola zakresu).
    #[inline]
    pub fn get(&self, x: usize, y: usize) -> bool {
        if x < WIDTH && y < HEIGHT {
            self.pix[y * WIDTH + x]
        } else {
            false
        }
    }

    /// Ustawia piksel (ignoruje poza zakresem).
    #[inline]
    pub fn set(&mut self, x: i32, y: i32, on: bool) {
        if x >= 0 && y >= 0 && (x as usize) < WIDTH && (y as usize) < HEIGHT {
            self.pix[y as usize * WIDTH + x as usize] = on;
        }
    }

    /// Wypelniony prostokat.
    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, on: bool) {
        for dy in 0..h {
            for dx in 0..w {
                self.set(x + dx, y + dy, on);
            }
        }
    }

    /// Obrys prostokata. (uzywane w renderze menu - M4)
    #[allow(dead_code)]
    pub fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, on: bool) {
        for dx in 0..w {
            self.set(x + dx, y, on);
            self.set(x + dx, y + h - 1, on);
        }
        for dy in 0..h {
            self.set(x, y + dy, on);
            self.set(x + w - 1, y + dy, on);
        }
    }

    /// Rysuje pojedynczy znak; `scale` powieksza glif (1 = 5x7).
    /// Zwraca szerokosc zajeta na ekranie (lacznie z 1px odstepem).
    pub fn draw_char(&mut self, x: i32, y: i32, c: char, scale: i32, on: bool) -> i32 {
        let g = font::glyph(c);
        for (col, bits) in g.iter().enumerate() {
            for row in 0..font::H {
                if (bits >> row) & 1 == 1 {
                    let px = x + col as i32 * scale;
                    let py = y + row as i32 * scale;
                    self.fill_rect(px, py, scale, scale, on);
                }
            }
        }
        (font::W as i32 + 1) * scale
    }

    /// Rysuje tekst od (x,y). Zwraca koncowe x.
    pub fn draw_text(&mut self, x: i32, y: i32, text: &str, scale: i32, on: bool) -> i32 {
        let mut cx = x;
        for c in text.chars() {
            cx += self.draw_char(cx, y, c, scale, on);
        }
        cx
    }

    /// Szerokosc tekstu w pikselach przy danej skali.
    pub fn text_width(text: &str, scale: i32) -> i32 {
        text.chars().count() as i32 * (font::W as i32 + 1) * scale
    }

    /// Rysuje tekst wysrodkowany w poziomie.
    pub fn draw_text_centered(&mut self, y: i32, text: &str, scale: i32, on: bool) {
        let w = Self::text_width(text, scale);
        let x = (WIDTH as i32 - w) / 2;
        self.draw_text(x, y, text, scale, on);
    }
}
