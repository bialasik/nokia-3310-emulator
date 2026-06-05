//! Model magistrali MBUS (szeregowa, akcesoria/serwis) — wg MADos hw/mbus.c.
//!
//! Rejestry (baza 0x20000): CTRL=0x18, STATUS=0x19, BYTE=0x1A.
//!  CTRL bity: TXD=0x20, RXD=0x40, RESET=0x80.
//!  STATUS bity: BITCNT=0x07, TXDRDY=0x10, RXDRDY=0x20, SCL=0x40, SDA=0x80.
//!
//! mbus_init: `CTRL=0x80; while(CTRL & 0x80);` — bit RESET musi SAM się wyzerować
//! (reset sprzętowy kończy się natychmiast). Bez podłączonego akcesorium magistrala
//! jest bezczynna: linie SCL/SDA wysokie (idle), gotowa do nadawania (TXDRDY), brak
//! odbioru (RXDRDY=0). To pozwala pętlom firmware'u przejść bez wieszania.

pub const REG_MBUS_CTRL: u32 = 0x0002_0018;
pub const REG_MBUS_STATUS: u32 = 0x0002_0019;
pub const REG_MBUS_BYTE: u32 = 0x0002_001A;

const MBUS_CTRL_RESET: u8 = 0x80;
const MBUS_STATUS_TXDRDY: u8 = 0x10;
const MBUS_STATUS_SCL: u8 = 0x40;
const MBUS_STATUS_SDA: u8 = 0x80;

#[derive(Default)]
pub struct Mbus {
    ctrl: u8,
    /// Ostatni zapisany bajt nadawczy (diagnostyka).
    last_byte: u8,
}

impl Mbus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Odczyt rejestru MBUS; None => nie nasz adres.
    pub fn read(&self, addr: u32) -> Option<u8> {
        Some(match addr {
            // CTRL: bit RESET(0x80) odczytujemy jako WYZEROWANY (reset HW natychmiast
            // zakończony) — inaczej `while(CTRL&0x80)` w mbus_init wisi.
            REG_MBUS_CTRL => self.ctrl & !MBUS_CTRL_RESET,
            // STATUS: bezczynna magistrala — gotowa do nadania, linie wysokie, brak RX.
            REG_MBUS_STATUS => MBUS_STATUS_TXDRDY | MBUS_STATUS_SCL | MBUS_STATUS_SDA,
            // BYTE: brak odebranych danych.
            REG_MBUS_BYTE => 0x00,
            _ => return None,
        })
    }

    /// Zapis rejestru MBUS; true => obsłużono.
    pub fn write(&mut self, addr: u32, val: u8) -> bool {
        match addr {
            REG_MBUS_CTRL => self.ctrl = val,
            REG_MBUS_STATUS => {} // bity statusu sterowane sprzętowo; zapisy ignorujemy
            REG_MBUS_BYTE => self.last_byte = val,
            _ => return false,
        }
        true
    }
}
