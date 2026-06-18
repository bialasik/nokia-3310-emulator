//! Matryca klawiatury DCT3/3310 — wierny model skanu stockowego firmware'u.
//!
//! Sprzęt: wiersze sterowane przez KPD_R (0x20028) i DIR_R (0x200a8 = IO_UIF_DIR_R),
//! kolumny czytane z KPD_C (0x2002A), aktywne-low (bit=0 => klawisz wciśnięty).
//!
//! Skan stocka (fcn.002e96a8) ma DWIE gałęzie:
//!  - "wszystkie wiersze naraz" (KPD_R=0x1f): jeśli któraś kolumna wciśnięta, zwraca
//!    kod `0x80 + numer_bitu_kolumny`. POWER zwiera bit 1 => kod 0x81 (oczekiwany przy
//!    boocie po naciśnięciu klawisza włączania).
//!  - per-wiersz (DIR_R = 1<<wiersz): mapuje (wiersz,kolumna) -> kod wg kpd_getkey_matrix.
//!
//! KEYMAP v6.39 (odczytany z firmware @0x33043c, NIE matryca MADos!). Skan 0x2e98f0 zwraca
//! indeks = wiersz_bit*5 + kolumna_bit (OBA wprost), caller fcn.002eccfc robi
//! `keycode = keymap[layer*25 + indeks]`. Tablica (warstwa 0, indeks -> firmware keycode):
//!        col0  col1  col2  col3  col4
//!   row0: 3e    17    0a    3e    1a
//!   row1: 3e    18    01    02    01
//!   row2: 3e    3e    06    05    04
//!   row3: 3e    3e    09    08    07
//!   row4: 3e    03    0b    19    0c
//! 0x3e = brak klawisza. Cyfry 1-9 = 0x01-0x09, 0 = 0x0a, * = 0x0b, # = 0x0c.
//! 0x17/0x18/0x19/0x1a = soft/nawigacja (do ustalenia empirycznie).
//! POWER = klawisz włączania: linia DEDYKOWANA col1 -> keymap dedyk @0x330458[1]=0x0d.

pub const REG_KPD_R: u32 = 0x0002_0028;
pub const REG_KPD_C: u32 = 0x0002_002A;
/// IO_UIF_DIR_R (0x200a8): driver wierszy matrycy. Skan pisze `1<<wiersz` (pojedynczy
/// wiersz) lub maskę górną (0xe0) / 0x1f (wszystkie) zależnie od fazy skanu.
pub const REG_DIR_R: u32 = 0x0002_00A8;

/// Firmware keycody v6.39 (z keymap @0x33043c). Wstepne przypisanie nawigacji
/// (0x17/0x18/0x19/0x1a) - do potwierdzenia testem.
pub const KEY_UP: u8 = 0x17; // bez wplywu na PIN (przewijanie)
pub const KEY_DOWN: u8 = 0x18; // bez wplywu na PIN (przewijanie)
pub const KEY_MENU: u8 = 0x19; // powoduje przejscie - kandydat OK/soft
pub const KEY_CANCEL: u8 = 0x1a; // usuwa cyfre PIN = C/Back (potwierdzone)
pub const KEY_SELECT: u8 = 0x19;

/// Flagi debug ze srodowiska cache'owane RAZ (OnceLock). std::env::var w hot-path (read_c
/// per skan gdy klawisz trzymany) jest wolny - zabija wydajnosc.
fn kpd_rc_flag() -> bool {
    static F: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *F.get_or_init(|| std::env::var("KPD_RC").is_ok())
}
fn kpd_dbg_flag() -> bool {
    static F: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *F.get_or_init(|| std::env::var("KPD_DBG").is_ok())
}

/// Pozycja klawisza w matrycy: (wiersz, bit_kolumny). bit_kolumny = 4 - col_idx.
/// Zwraca None dla nieistniejących kodów.
fn matrix_pos(code: u8) -> Option<(u8, u8)> {
    // KEYMAP v6.39 odczytany z firmware @0x33043c (warstwa 0). idx = wiersz_bit*5 + kol_bit.
    const M: [u8; 25] = [
        0x3e, 0x17, 0x0a, 0x3e, 0x1a, // row0: -, soft, 0, -, C/soft
        0x3e, 0x18, 0x01, 0x02, 0x01, // row1: -, soft, 1, 2, 1
        0x3e, 0x3e, 0x06, 0x05, 0x04, // row2: -, -, 6, 5, 4
        0x3e, 0x3e, 0x09, 0x08, 0x07, // row3: -, -, 9, 8, 7
        0x3e, 0x03, 0x0b, 0x19, 0x0c, // row4: -, 3, *, nav, #
    ];
    M.iter().position(|&c| c == code).map(|idx| {
        let row = (idx / 5) as u8;
        let col_idx = (idx % 5) as u8;
        // DEFINITYWNIE z trace skanu idle 0x2e98f0: firmware czyta
        // matrix[driven_row_bit * 5 + cleared_KPD_C_bit] — OBA WPROST (bez inwersji).
        // Czyli klawisz matrix[R][C] prezentujemy na (wiersz R, bit kolumny C) = col_idx.
        (row, col_idx)
    })
}

/// WSZYSTKIE pozycje matrycy dla danego kodu. Klawisz "1" (0x01) wystepuje w DWoCH miejscach
/// keymapu (row1,col2 i row1,col4) - fizyczny klawisz zwiera obie kolumny. Prezentacja tylko
/// jednej dawala firmware niespojny stan -> "1" zachowywal sie jak long-press (ksiazka tel.).
fn matrix_positions(code: u8) -> Vec<(u8, u8)> {
    const M: [u8; 25] = [
        0x3e, 0x17, 0x0a, 0x3e, 0x1a,
        0x3e, 0x18, 0x01, 0x02, 0x01,
        0x3e, 0x3e, 0x06, 0x05, 0x04,
        0x3e, 0x3e, 0x09, 0x08, 0x07,
        0x3e, 0x03, 0x0b, 0x19, 0x0c,
    ];
    M.iter().enumerate()
        .filter(|(_, &c)| c == code)
        .map(|(idx, _)| ((idx / 5) as u8, (idx % 5) as u8))
        .collect()
}

pub struct Keypad {
    kpd_r: u8,
    /// IO_UIF_DIR_R (0x200a8): maska sterowanych wierszy ze skanu.
    dir_r: u8,
    steps: u64,
    pwr_hold: u64,
    /// Czy klawisz POWER (włączania) jest wciśnięty (zwiera bit kolumny 1 => kod 0x81).
    power_pressed: bool,
    /// Wciśnięte klawisze jako pozycje matrycy (wiersz, bit_kolumny).
    pressed: Vec<(u8, u8)>,
    /// Diagnostyka: licznik odczytow KPD_C (czy skan klawiatury w ogole dziala).
    pub read_c_count: std::cell::Cell<u64>,
    /// Diagnostyka: licznik odczytow w ktorych zwrocono != 0x7F (wykryto klawisz).
    pub read_c_hit: std::cell::Cell<u64>,
    /// Oczekujace przerwanie klawiatury (IRQL bit0): sprzet asertuje IRQ gdy linia
    /// kolumny opadnie (klawisz wcisniety/zwolniony). Handler 0x2e9844 ackuje 0x2006b
    /// i skanuje matryce. Bez tego firmware NIGDY nie skanuje klawiatury w idle.
    pub key_irq_pending: bool,
    /// KEYLOG: loguj skany w ktorych firmware widzi klawisz (cols != 0x7f) - co i kiedy widzi.
    key_log: bool,
    /// Anty-auto-repeat (event-driven): po ZAREJESTROWANIU eventu nowego klawisza przez firmware
    /// (zapis kodu do 0x111b6f) prezentujemy klawisz matrycy jako PUSZCZONY mimo trzymania -
    /// firmware nie wchodzi w petle repeatu (~66 skanow). Czyszczone przy fizycznym puszczeniu.
    suppress: std::cell::Cell<bool>,
}

impl Default for Keypad {
    fn default() -> Self {
        Self::new()
    }
}

impl Keypad {
    pub fn new() -> Self {
        // 3310 wymaga przytrzymania POWER ~1.5 s dla potwierdzenia włączenia
        // (1.5 s @ 13 MHz ~= 19.5M kroków); potem POWER puszczony.
        let pwr_hold = std::env::var("PWR_HOLD_STEPS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(19_500_000);
        Self {
            kpd_r: 0,
            dir_r: 0,
            steps: 0,
            pwr_hold,
            power_pressed: false,
            pressed: Vec::new(),
            read_c_count: std::cell::Cell::new(0),
            read_c_hit: std::cell::Cell::new(0),
            key_irq_pending: false,
            key_log: std::env::var("KEYLOG").is_ok(),
            suppress: std::cell::Cell::new(false),
        }
    }

    /// Firmware zarejestrowal event nowego klawisza -> puszczamy matryce (anty-repeat).
    /// Wlaczane env KPD_ANTIREPEAT (domyslnie wl.); KPD_ANTIREPEAT=0 wylacza.
    pub fn suppress_after_event(&self) {
        self.suppress.set(true);
    }

    pub fn tick(&mut self, cycles: u32) {
        self.steps += cycles as u64;
    }

    pub fn write_r(&mut self, val: u8) {
        self.kpd_r = val;
    }

    /// Zapis IO_UIF_DIR_R (0x200a8): maska sterowanego wiersza/-y.
    pub fn write_dir_r(&mut self, val: u8) {
        self.dir_r = val;
    }

    /// POWER przytrzymany przy starcie (auto, pierwsze `pwr_hold` kroków) lub ręcznie.
    fn power_active(&self) -> bool {
        self.power_pressed || self.steps < self.pwr_hold
    }

    /// Czy skan adresuje pojedynczy wiersz. Sprzęt: DIR_R ustawia kierunek (które linie
    /// wierszy są wyjściami), KPD_R steruje nimi aktywnie-low (wyczyszczony bit = wiersz
    /// napędzony). Aktywny wiersz = DIR_R & !KPD_R (oba zgodne). v6.39 przy skanie per-wiersz
    /// trzyma DIR_R=0x1F i zmienia KPD_R (0xFE/0xFD/...); stary kod patrzył tylko na DIR_R
    /// i mylił to z trybem all-rows. Wiele/zero bitów aktywnych = tryb all-rows.
    #[inline]
    fn single_row(&self) -> Option<u8> {
        let active = self.dir_r & !self.kpd_r & 0x1F;
        if active.is_power_of_two() {
            Some(active.trailing_zeros() as u8)
        } else {
            None
        }
    }

    /// Odczyt kolumn KPD_C. Sprzęt ma 7 linii (MADos: KPD_C=0x7F = nic wciśnięte);
    /// bity 0..4 to kolumny matrycy (aktywne-low), bity 5,6 niewykorzystane (stałe 1).
    /// Stan "nic" = 0x7F (nie 0x1F) — firmware czeka na 0x7F w kpd_wait_release.
    pub fn read_c(&self) -> u8 {
        self.read_c_count.set(self.read_c_count.get() + 1);
        let mut cols = 0x7Fu8;
        match self.single_row() {
            // Tryb per-wiersz: klawisze z wiersza, o ile nie tlumimy po zarejestrowanym evencie.
            Some(row) if !self.suppress.get() => {
                for &(r, colbit) in &self.pressed {
                    if r == row {
                        cols &= !(1 << colbit);
                    }
                }
            }
            Some(_) => {} // suppress: matryca prezentowana jako puszczona (anty-repeat)
            // Tryb "wszystkie wiersze naraz" (DIR_R=0x1f/0xe0/0): firmware używa go do
            // detekcji linii DEDYKOWANYCH (POWER = bit kolumny 1 => kod 0x81), NIE matrycy.
            // Scan 0x2e98f0 robi pre-check (DIR_R=0xe0); gdy all-rows == 0x1f (brak
            // dedykowanego) -> dopiero wtedy pełny scan per-wiersz identyfikuje klawisz
            // matrycy. Dlatego klawisze matrycy NIE mogą tu wystąpić (inaczej FW pomija
            // scan per-row i nie rozpoznaje klawisza - błąd "tylko POWER działa w idle").
            None => {
                if self.power_active() {
                    cols &= !(1 << 1);
                }
            }
        }
        if cols != 0x7F {
            self.read_c_hit.set(self.read_c_hit.get() + 1);
            if self.key_log {
                // Ktora kolumna opadla (klawisz) + wiersz -> identyfikuje klawisz; steps=cykle.
                eprintln!("[fw_scan] row={:?} cols={:#04x} cyc={}", self.single_row(), cols, self.steps);
            }
        }
        if !self.pressed.is_empty() && kpd_rc_flag() {
            eprintln!(
                "[rc] step={} dir_r={:#04x} kpd_r={:#04x} row={:?} pressed={:?} -> cols={:#04x}",
                self.steps, self.dir_r, self.kpd_r, self.single_row(), self.pressed, cols
            );
        }
        cols
    }

    /// Wciśnięcie klawisza po kodzie (KEY_UP/DOWN/MENU/CANCEL lub cyfra wg matrycy).
    pub fn press_code(&mut self, code: u8) {
        let positions = matrix_positions(code);
        if positions.is_empty() {
            if kpd_dbg_flag() {
                eprintln!("[kpd] press {:#04x} -> BRAK pozycji w matrycy!", code);
            }
            return;
        }
        // Nowe nacisniecie z pustego stanu = czysc tlumienie (nowy event moze sie zarejestrowac).
        if self.pressed.is_empty() {
            self.suppress.set(false);
        }
        // Zwieramy WSZYSTKIE kolumny przypisane do kodu (klawisz "1" zwiera dwie).
        for pos in positions {
            if !self.pressed.contains(&pos) {
                self.pressed.push(pos);
                self.key_irq_pending = true; // opadajaca linia kolumny -> IRQ klawiatury
                if kpd_dbg_flag() {
                    eprintln!("[kpd] press {:#04x} -> {:?} (irq set)", code, pos);
                }
            }
        }
    }
    pub fn release_code(&mut self, code: u8) {
        for pos in matrix_positions(code) {
            if self.pressed.iter().any(|&k| k == pos) {
                self.pressed.retain(|&k| k != pos);
                self.key_irq_pending = true; // zmiana stanu linii -> IRQ (detekcja zwolnienia)
            }
        }
        // Fizyczne puszczenie (matryca pusta) -> zeruj tlumienie (kolejny nacisk zadziala).
        if self.pressed.is_empty() {
            self.suppress.set(false);
        }
    }

    /// Sterowanie klawiszem POWER (włączania).
    pub fn set_power(&mut self, down: bool) {
        if self.power_pressed != down {
            self.key_irq_pending = true;
        }
        self.power_pressed = down;
    }

    /// Pobiera i kasuje oczekujace przerwanie klawiatury (edge-triggered).
    pub fn take_key_irq(&mut self) -> bool {
        let p = self.key_irq_pending;
        self.key_irq_pending = false;
        p
    }

    pub fn release_all(&mut self) {
        self.pressed.clear();
        self.power_pressed = false;
    }
}
