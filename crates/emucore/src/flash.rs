//! Model układu flash (Intel/ST CFI, 16-bit) — wg MADos hw/flash.c.
//!
//! Firmware (i FFS/PM) zapisuje przez sekwencje komend Intel-style do regionu flash
//! (0x200000+): erase 0x20/0xD0, program 0x40 (lub 0x10), clear-status 0x50,
//! read-status 0x70, read-id 0x90, read-array 0xFF. Status: bit7 (0x80) = WSM ready.
//! flash.c kopiuje rutynę do RAM i wykonuje stamtąd (bo flash w trybie program/erase
//! nie daje się czytać jako tablica) — więc nasz globalny tryb status/id NIE psuje
//! pobierania instrukcji (PC jest w RAM podczas operacji). Po operacji firmware pisze
//! 0xFF (read-array).
//!
//! WAŻNE: operujemy na KOPII ROM w pamięci emulatora (Machine.rom) — plik .fls na dysku
//! NIE jest modyfikowany (nigdy nie zapisujemy z powrotem). Program: bity tylko 1->0
//! (NOR). Erase: blok wyrównany do 64KB -> 0xFF. Operacje kończą się natychmiast (ready).

#[derive(Clone, Copy, PartialEq)]
enum Cmd {
    Array,     // tryb tablicy (normalne czytanie danych/kodu)
    Status,    // odczyt rejestru statusu (bit7=ready)
    Id,        // odczyt identyfikatora producenta/urzadzenia
    EraseSetup,   // po 0x20, czeka na 0xD0 (potwierdzenie)
    ProgramSetup, // po 0x40/0x10, czeka na slowo danych
}

/// Operacja na obrazie ROM zwracana przez write16 (magistrala ją wykonuje, by uniknąć
/// podwójnego pożyczenia self.flash + self.rom).
pub enum FlashOp {
    None,
    /// Programuj słowo 16-bit pod offsetem (bity 1->0): rom[off..off+2] &= val (BE).
    Program(usize, u16),
    /// Wymaż blok: wypełnij [start, start+len) wartością 0xFF.
    Erase(usize, usize),
}

const BLOCK_SIZE: usize = 0x1_0000; // 64KB blok (wyrównanie erase)
const STATUS_READY: u8 = 0x80; // bit7 WSM ready
// ID: ST M58 (firmware sprawdza 0x20=ST). Producent=0x20, urzadzenie dowolne.
const MANUF_ID: u8 = 0x20;
const DEVICE_ID: u8 = 0xA0;

pub struct Flash {
    cmd: Cmd,
    erase_off: usize,
}

impl Default for Flash {
    fn default() -> Self {
        Self::new()
    }
}

impl Flash {
    pub fn new() -> Self {
        Self {
            cmd: Cmd::Array,
            erase_off: 0,
        }
    }

    /// Czy flash jest w trybie tablicy (zwykłe czytanie ROM). Gdy false, odczyty regionu
    /// flash zwracają status/ID zamiast danych.
    #[inline]
    pub fn is_array(&self) -> bool {
        self.cmd == Cmd::Array
    }

    /// Bajt zwracany przy odczycie regionu flash w trybie nie-tablicowym.
    /// `off` = offset względem bazy flash (0x200000), parzysty/nieparzysty bez znaczenia.
    pub fn read_override(&self, off: usize) -> u8 {
        match self.cmd {
            Cmd::Array => unreachable!("read_override tylko poza Array"),
            // Status: bit7=ready na obu bajtach (firmware czyta ldrh, sprawdza &0x80).
            Cmd::Status | Cmd::EraseSetup | Cmd::ProgramSetup => STATUS_READY,
            // Id: offset 0 (×2=0) -> producent, offset 1 (addr 2) -> urzadzenie.
            Cmd::Id => {
                if (off & 0x2) == 0 {
                    MANUF_ID
                } else {
                    DEVICE_ID
                }
            }
        }
    }

    /// Zapis 16-bit komendy/danych do flash (off = addr - 0x200000). Zwraca operację
    /// na obrazie ROM do wykonania przez magistralę.
    pub fn write16(&mut self, off: usize, val: u16) -> FlashOp {
        let c = (val & 0xFF) as u8;
        match self.cmd {
            Cmd::EraseSetup => {
                if c == 0xD0 {
                    // potwierdzenie erase: wymaż blok zawierający `off`.
                    let block = off & !(BLOCK_SIZE - 1);
                    self.cmd = Cmd::Status;
                    return FlashOp::Erase(block, BLOCK_SIZE);
                }
                self.cmd = Cmd::Array; // przerwane
                FlashOp::None
            }
            Cmd::ProgramSetup => {
                self.cmd = Cmd::Status;
                FlashOp::Program(off, val)
            }
            _ => {
                // tryby Array/Status/Id — interpretuj komendę.
                match c {
                    0xFF => self.cmd = Cmd::Array,
                    0x90 => self.cmd = Cmd::Id,
                    0x70 => self.cmd = Cmd::Status,
                    0x50 => self.cmd = Cmd::Status, // clear status (ready)
                    0x20 => {
                        self.cmd = Cmd::EraseSetup;
                        self.erase_off = off;
                    }
                    0x40 | 0x10 => self.cmd = Cmd::ProgramSetup,
                    _ => {}
                }
                FlashOp::None
            }
        }
    }
}
