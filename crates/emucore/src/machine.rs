//! Magistrala emulatora: implementuje `arm7tdmi::MemoryInterface`.
//!
//! Mapa pamieci DCT3/3310 (ustalona z MADos `data/3310_old/memmap` + dezasemblacji entry):
//!   0x00000000..0x00010000  RAM niska / wektory (backed)
//!   0x00010000..0x00050000  MMIO (bloki 0x10000/0x20000/0x30000/0x40000), baza CTSI=0x20000
//!   0x00050000..0x00200000  RAM (m.in. 0x100000.. wg memmap; stos schodzi od ~0x200000)
//!   0x00200000..0x00400000  ROM (firmware+PPM+EEPROM, obraz z .fls)
//!
//! MMIO domyslnie czyta 0xFF (bity "ready" ustawione -> spin-waity `io_wait` przechodza),
//! zapisy sa pamietane. Kazdy dostep do MMIO/nieznanego jest logowany (bring-up).

use crate::ccont::{Ccont, REG_CC_RD, REG_CC_WR};
use crate::ctsi::Ctsi;
use crate::dsp::{Dsp, DSP_UPLOADREPLY, REG_CTSI_DSP};
use crate::flash::{Flash, FlashOp};
use crate::keypad::{Keypad, REG_DIR_R, REG_KPD_C, REG_KPD_R};
use crate::lcd::{Pcd8544, REG_LCD_CMD, REG_LCD_DATA};
use crate::mbus::Mbus;
use crate::periph::Periph;

/// Flaga debug ze srodowiska czytana RAZ (OnceLock cache). std::env::var robi syscall +
/// alokacje - wolany w hot-path (raw_read/raw_write co krok) zabija wydajnosc (13MHz -> 5MHz).
fn dbg_flag(cell: &std::sync::OnceLock<bool>, name: &str) -> bool {
    *cell.get_or_init(|| std::env::var(name).is_ok())
}

/// VWATCH (env): wartosc do zlapania przy zapisie 16/32-bit (np. ID komunikatu). Cache.
fn vwatch_val() -> Option<u32> {
    static V: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        std::env::var("VWATCH")
            .ok()
            .and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok())
    })
}
use crate::sim::{Sim, SIM_BASE, SIM_END};
use arm7tdmi::memory::{Addr, BusIO, DebugRead, MemoryAccess, MemoryInterface};
use std::collections::HashMap;

pub const RAM_SIZE: usize = 0x0020_0000; // 0x000000..0x200000
pub const ROM_START: u32 = 0x0020_0000;
pub const ROM_END: u32 = 0x0040_0000;
pub const MMIO_START: u32 = 0x0001_0000;
pub const MMIO_END: u32 = 0x0005_0000;
pub const MMIO_SIZE: usize = (MMIO_END - MMIO_START) as usize;

/// Punkt wejscia firmware (za 0x40-bajtowym naglowkiem flasha).
pub const FW_ENTRY: u32 = 0x0020_0040;
/// Baza rejestrow systemowych (CTSI/PUP/UIF/GENSIO).
pub const IO_BASE: u32 = 0x0002_0000;
/// IO_CTSI_RST (MADos ioports.h IO_CTSI_RST=0x01): rejestr resetu/kontroli CTSI.
/// Bit 2 (0x04) wpisany przez firmware = programowy reset CPU (skok do FW_ENTRY).
pub const REG_CTSI_RST: u32 = 0x0002_0001;

/// Pojedynczy zarejestrowany dostep do MMIO / regionu nieznanego.
#[derive(Clone, Copy)]
pub struct Access {
    pub pc: u32,
    pub write: bool,
    pub width: u8,
    pub addr: u32,
    pub value: u32,
}

pub struct Machine {
    rom: Box<[u8]>,   // 0x200000..0x400000
    ram: Box<[u8]>,   // 0x000000..0x200000
    mmio: Box<[u8]>,  // 0x010000..0x050000 (domyslnie 0xFF)
    pub pc_hint: u32,
    pub trace: Vec<Access>,
    pub trace_limit: usize,
    pub mmio_reads: u64,
    pub mmio_writes: u64,
    pub rom_writes: u64,
    pub unmapped: u64,
    /// Histogram odczytow MMIO wg adresu (wykrywanie busy-poll).
    pub read_hist: HashMap<u32, u64>,
    /// Adres do obserwacji (watch): zapisuje (pc, szer, wartosc) przy odczycie.
    pub watch: Option<u32>,
    pub watch_hits: Vec<(u32, u8, u32)>,
    /// Kontroler LCD PCD8544 (dekoduje zapisy GENSIO -> bufor 84x48).
    pub lcd: Pcd8544,
    /// Kontroler przerwan + timer (CTSI).
    pub ctsi: Ctsi,
    /// Uklad zasilania CCONT (przez GENSIO).
    pub ccont: Ccont,
    /// Magistrala MBUS (akcesoria/serwis).
    pub mbus: Mbus,
    /// Interfejs karty SIM (UART ISO-7816).
    pub sim: Sim,
    /// Peryferia wyjsciowe: brzeczyk, wibracja, LED, watchdog.
    pub periph: Periph,
    /// Model DSP (TMS320 baseband): boot/upload handshake + gotowosc (IRQ_DSP).
    pub dsp: Dsp,
    /// Uklad flash (Intel/ST CFI): komendy erase/program/status na kopii ROM w pamieci.
    pub flash: Flash,
    /// PC ostatnich zapisow do LCD (do namierzenia rutyny rysujacej).
    pub lcd_last_cmd_pc: u32,
    pub lcd_last_data_pc: u32,
    /// Matryca klawiatury (POWER przy boocie + nawigacja).
    pub keypad: Keypad,
    /// Write-watch (env WWATCH): loguje (pc, wartosc) przy KAZDYM zapisie bajtu pod ten adres.
    pub wwatch: Option<u32>,
    pub wwatch_hits: Vec<(u32, u8)>,
    /// Licznik wymuszen SIM_GATE (celowany hack bramki reject).
    sim_gate_cnt: u32,
    /// Programowy reset CPU zazadany (zapis bitu 2 do IO_CTSI_RST). Pętla wykonania
    /// wykrywa flage i wykonuje soft-reset (PC=FW_ENTRY). Licznik = ile resetow.
    pub reset_request: bool,
    pub reset_count: u32,
    /// Diagnostyka: ile razy zaasertowano przerwanie klawiatury (IRQL bit0).
    pub key_irq_asserts: u64,
    /// TEST heartbeat (env WAKE_PERIOD/WAKE_ADDR): co N krokow zeruj flage uspienia.
    wake_period: u64,
    wake_addr: u32,
    wake_steps: u64,
    /// TEST wstrzykiwania przerwan (env INJECT_IRQ/INJECT_FIQ maska, INJECT_PERIOD krokow):
    /// cyklicznie ustawia bity latcha IRQ/FIQ - probuje odblokowac async self-test.
    inject_irq: u8,
    inject_fiq: u8,
    inject_period: u64,
    /// Globalny licznik kroków (tick_timer).
    tick_count: u64,
    /// TEST (env FORCE_B2_AFTER): po N krokach czyść bit2 flagi 0x11ff15 przy odczycie
    /// (po ukończeniu buildu self-testu, gdy dane już ustawione) - opóźnione obejście.
    force_b2_after: u64,
    /// Test-bypass self-testu (env FORCE_ST): wymusza bit7 flagi 0x11ff15.
    force_st: bool,
    st_pass: bool,
    reg_all: bool,
    selftest_sub: bool,
    force_hw: bool,
    force_rdy: bool,
}

impl Machine {
    /// `image` to obraz flasha 0x000000..0x400000; bierzemy z niego region ROM.
    pub fn new(image: Vec<u8>) -> Self {
        let mut img = image;
        img.resize(ROM_END as usize, 0xFF);
        let rom = img[ROM_START as usize..ROM_END as usize].to_vec();
        Self {
            rom: rom.into_boxed_slice(),
            ram: vec![0u8; RAM_SIZE].into_boxed_slice(),
            mmio: vec![0xFFu8; MMIO_SIZE].into_boxed_slice(),
            pc_hint: 0,
            trace: Vec::new(),
            trace_limit: 512,
            mmio_reads: 0,
            mmio_writes: 0,
            rom_writes: 0,
            unmapped: 0,
            read_hist: HashMap::new(),
            watch: None,
            watch_hits: Vec::new(),
            lcd: Pcd8544::new(),
            ctsi: Ctsi::new(),
            ccont: Ccont::new(),
            mbus: Mbus::new(),
            sim: Sim::new(),
            periph: Periph::new(),
            dsp: Dsp::new(),
            flash: Flash::new(),
            lcd_last_cmd_pc: 0,
            lcd_last_data_pc: 0,
            keypad: Keypad::new(),
            wwatch: std::env::var("WWATCH").ok().and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok()),
            wwatch_hits: Vec::new(),
            sim_gate_cnt: 0,
            reset_request: false,
            reset_count: 0,
            key_irq_asserts: 0,
            wake_period: std::env::var("WAKE_PERIOD").ok().and_then(|s| s.parse().ok()).unwrap_or(0),
            wake_addr: std::env::var("WAKE_ADDR").ok().and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok()).unwrap_or(0),
            wake_steps: 0,
            inject_irq: std::env::var("INJECT_IRQ").ok().and_then(|s| u8::from_str_radix(s.trim_start_matches("0x"), 16).ok()).unwrap_or(0),
            inject_fiq: std::env::var("INJECT_FIQ").ok().and_then(|s| u8::from_str_radix(s.trim_start_matches("0x"), 16).ok()).unwrap_or(0),
            inject_period: std::env::var("INJECT_PERIOD").ok().and_then(|s| s.parse().ok()).unwrap_or(100000),
            tick_count: 0,
            force_b2_after: std::env::var("FORCE_B2_AFTER").ok().and_then(|s| s.parse().ok()).unwrap_or(0),
            force_st: std::env::var("FORCE_ST").is_ok(),
            st_pass: std::env::var("ST_PASS").is_ok(),
            reg_all: std::env::var("REG_ALL").is_ok(),
            selftest_sub: std::env::var("SELFTEST_SUB").is_ok(),
            force_hw: std::env::var("FORCE_HW").is_ok(),
            force_rdy: std::env::var("FORCE_RDY").is_ok(),
        }
    }

    /// Krok urzadzen czasowych (timer CTSI + klawiatura) - wolac co krok CPU.
    /// Czysty odczyt 32-bit big-endian z RAM (skan stosu/debug) - BEZ efektow ubocznych
    /// (watch/record/dsp). Firmware DCT3 = BIG-ENDIAN, slowa na stosie tez.
    pub fn peek_ram32(&self, addr: u32) -> u32 {
        let a = (addr & !3) as usize;
        if a + 3 < self.ram.len() {
            u32::from_be_bytes([self.ram[a], self.ram[a + 1], self.ram[a + 2], self.ram[a + 3]])
        } else {
            0
        }
    }

    pub fn tick_timer(&mut self) {
        self.ctsi.tick();
        self.keypad.tick();
        self.ccont.tick(); // zegar RTC CCONT tika
        self.tick_count = self.tick_count.wrapping_add(1);
        // TEST heartbeat: cyklicznie zeruj flage uspienia (wybudza petle idle bez efektow
        // ubocznych przerwania). Diagnoza hipotezy "startup potrzebuje cyklicznego ticka".
        if self.wake_period > 0 {
            self.wake_steps = self.wake_steps.wrapping_add(1);
            if self.wake_steps % self.wake_period == 0 && (self.wake_addr as usize) < RAM_SIZE {
                self.ram[self.wake_addr as usize] = 0;
            }
        }
        // TEST: cykliczne wstrzykiwanie przerwan (proba odblokowania async self-testu).
        if (self.inject_irq != 0 || self.inject_fiq != 0) && self.inject_period > 0 {
            self.wake_steps = self.wake_steps.wrapping_add(1);
            if self.wake_steps % self.inject_period == 0 {
                self.ctsi.irq_latch |= self.inject_irq;
                self.ctsi.fiq_latch |= self.inject_fiq;
            }
        }
        // Przerwanie klawiatury (IRQL bit0): naciśniecie/zwolnienie klawisza opuszcza
        // linie kolumny -> sprzet asertuje IRQ. Handler 0x2e9844 ackuje 0x2006b i skanuje
        // matryce (0x2ec9aa). Bez tego firmware nie skanuje klawiatury w idle.
        if self.keypad.take_key_irq() {
            self.ctsi.irq_latch |= 1 << 0;
            self.key_irq_asserts += 1;
        }
        // SIM UART RX (FIQ bit6 = SIMUART): po aktywacji karty (CTRL bit7) SIM wysyla ATR
        // bajt-po-bajcie. Gdy bajt gotowy -> assert FIQ bit6; ISR czyta RXD (0x20037).
        // To pierwszy krok emulacji karty SIM (ATR) - by telefon wszedl poza "Wloz SIM".
        if self.sim.rx_tick() {
            self.ctsi.fiq_latch |= 1 << 6;
        }
        // Model DSP: po WLACZENIU DSP (IO_CTSI_DSP bit0, wykrywane w raw_write8) odlicza
        // i zglasza gotowosc -> IRQL bit4 (IRQ_DSP -> handler 0x2bccac) + UPLOADREPLY=0
        // (warunek handlera by ustawic flage gotowosci 0x1106e8=1).
        match self.dsp.tick() {
            Some(crate::dsp::DspAction::Ready) => {
                self.ctsi.irq_latch |= 1 << 4;
                self.mmio_w16(DSP_UPLOADREPLY, 0x0000);
            }
            Some(crate::dsp::DspAction::MailboxConsumed) => {
                // DSP skonsumowal komende MDI -> wyczysc mailbox 0x100e0 (baseband moze
                // wyslac kolejna komende L1). Patrz dsp.rs on_dspif_write.
                self.mmio_w16(crate::dsp::DSP_MDI_MAILBOX, 0x0000);
                // DSP_MDI_REPLY (env): po skonsumowaniu komendy L1 (post-PIN), DSP odpowiada przez
                // kolejke MDIRCV (wg MADos mdi.c) + FIQ_MDIRCV. Walidacja transportu: czy handler
                // receive firmware sie uruchamia i czyta kolejke. Typ testowy 0x00, pusty payload.
                static MR: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
                if dbg_flag(&MR, "DSP_MDI_REPLY") && self.tick_count > 33_000_000 {
                    // Odpowiedz przez MDIRCV na kazdy kick. NIE konsumujemy MDISND (firmware sam nim
                    // zarzadza; konsumpcja desynchronizowala kolejke send). Typ: env DSP_MDI_TYPE (def 0).
                    // Payload: env DSP_MDI_PAY="w0,w1,..." (slowa hex), domyslnie pusty.
                    let typ = std::env::var("DSP_MDI_TYPE").ok().and_then(|s| u8::from_str_radix(s.trim_start_matches("0x"), 16).ok()).unwrap_or(0);
                    let pay: Vec<u16> = std::env::var("DSP_MDI_PAY").ok().map(|s| s.split(',').filter_map(|x| u16::from_str_radix(x.trim().trim_start_matches("0x"), 16).ok()).collect()).unwrap_or_default();
                    self.mdi_send_reply(typ, &pay);
                }
            }
            None => {}
        }
    }

    /// Zapis 16-bit (LE w shared mem DSP) do regionu MMIO - pomocniczy dla modelu DSP.
    fn mmio_w16(&mut self, addr: u32, val: u16) {
        if (MMIO_START..MMIO_END).contains(&addr) {
            let off = (addr - MMIO_START) as usize;
            // DSP shared mem: slowa little-endian (firmware czyta ldrh).
            self.mmio[off] = val as u8;
            self.mmio[off + 1] = (val >> 8) as u8;
        }
    }

    /// Odczyt slowa shared-mem DSP BIG-ENDIAN (jak firmware ldrh: high bajt na nizszym adresie).
    /// byte_addr = adres w MMIO (np. 0x101C8 = MDIRCV_TAIL).
    fn dsp_r16(&self, byte_addr: u32) -> u16 {
        let off = (byte_addr - MMIO_START) as usize;
        ((self.mmio[off] as u16) << 8) | self.mmio[off + 1] as u16
    }
    /// Zapis slowa shared-mem DSP BIG-ENDIAN.
    fn dsp_w16(&mut self, byte_addr: u32, val: u16) {
        let off = (byte_addr - MMIO_START) as usize;
        self.mmio[off] = (val >> 8) as u8;
        self.mmio[off + 1] = val as u8;
    }

    /// Czyta komende L1 z kolejki MDISND (MCU->DSP, wg MADos): od HEAD slowo kontrolne {rozmiar,typ}
    /// + payload, advance HEAD (ring). Zwraca (typ, payload). DSP konsumuje komendy by firmware mogl
    /// wysylac kolejne (free = head-tail). QUEUE=0x00, QUEUEEND=0x52, SIZE=0x52, HEAD@0x100A6, TAIL@0x100A4.
    fn mdi_recv_command(&mut self) -> Option<(u8, Vec<u16>)> {
        const QUEUE: u16 = 0x00;
        const QEND: u16 = 0x52;
        const QSIZE: u16 = 0x52;
        const HEAD_ADDR: u32 = 0x0001_00A6;
        const TAIL_ADDR: u32 = 0x0001_00A4;
        const BASE: u32 = 0x0001_0000;
        let mut h = self.dsp_r16(HEAD_ADDR);
        let tail = self.dsp_r16(TAIL_ADDR);
        if h == tail { return None; } // pusto
        if h >= QEND { h = QUEUE; }
        let ctrl = self.dsp_r16(BASE + h as u32 * 2);
        let size_bytes = (ctrl >> 8) as usize;
        let typ = (ctrl & 0xFF) as u8;
        let words = size_bytes.div_ceil(2);
        h += 1; if h >= QEND { h -= QSIZE; }
        let mut payload = Vec::with_capacity(words);
        for _ in 0..words {
            payload.push(self.dsp_r16(BASE + h as u32 * 2));
            h += 1; if h >= QEND { h -= QSIZE; }
        }
        self.dsp_w16(HEAD_ADDR, h);
        Some((typ, payload))
    }

    /// Wysyla raport L1 do firmware przez kolejke MDIRCV (wg MADos mdi.c, offsety potwierdzone
    /// w 6.39). Slowo kontrolne {rozmiar_bajtow_payload, typ} + payload (slowa) od TAIL, advance
    /// TAIL (ring buffer), wyzwala FIQ_MDIRCV. Firmware (head!=tail) odczyta i zdispatchuje wg typu.
    /// Layout (slowa _dsp[]): QUEUE=0x80 (byte 0x10100), QUEUEEND=0xE4, SIZE=0x64, TAIL@0x101C8, HEAD@0x101CA.
    fn mdi_send_reply(&mut self, msg_type: u8, payload: &[u16]) {
        const QUEUE: u16 = 0x80;
        const QEND: u16 = 0xE4;
        const QSIZE: u16 = 0x64;
        const TAIL_ADDR: u32 = 0x0001_01C8;
        const BASE: u32 = 0x0001_0000;
        let mut t = self.dsp_r16(TAIL_ADDR);
        if t < QUEUE || t >= QEND { t = QUEUE; } // bezpiecznik na niezainicjowany/zly TAIL
        // slowo kontrolne: high bajt = rozmiar payload w bajtach, low bajt = typ MDI.
        let ctrl = (((payload.len() as u16) * 2) << 8) | msg_type as u16;
        self.dsp_w16(BASE + t as u32 * 2, ctrl);
        t += 1; if t >= QEND { t -= QSIZE; }
        for &pw in payload {
            self.dsp_w16(BASE + t as u32 * 2, pw);
            t += 1; if t >= QEND { t -= QSIZE; }
        }
        self.dsp_w16(TAIL_ADDR, t);
        self.ctsi.fiq_latch |= 1 << 0; // FIQ_MDIRCV = bit0 (MADos int.h; bit1=MDISND ack przez send)
        if std::env::var("MDI_LOG").is_ok() {
            eprintln!("[mdi_reply typ={msg_type:#04x} payload={} slow -> TAIL={t:#x} @tick={}]", payload.len(), self.tick_count);
        }
    }
    /// Zapis do regionu flash (komenda lub dane). Interpretuje FSM flash i wykonuje
    /// operacje (program/erase) na KOPII ROM w pamieci (plik na dysku nietkniety).
    fn flash_write(&mut self, addr: u32, val: u16) {
        self.rom_writes += 1;
        let off = (addr - ROM_START) as usize;
        match self.flash.write16(off, val) {
            FlashOp::None => {}
            FlashOp::Program(o, v) => {
                // NOR: bity tylko 1->0. Zapis BIG-ENDIAN (jak reszta magistrali).
                if o + 1 < self.rom.len() {
                    let b = v.to_be_bytes();
                    self.rom[o] &= b[0];
                    self.rom[o + 1] &= b[1];
                }
            }
            FlashOp::Erase(start, len) => {
                let end = (start + len).min(self.rom.len());
                if start < end {
                    for b in &mut self.rom[start..end] {
                        *b = 0xFF;
                    }
                }
            }
        }
    }

    pub fn fiq_active(&self) -> bool {
        self.ctsi.fiq_active()
    }
    pub fn irq_active(&self) -> bool {
        self.ctsi.irq_active()
    }
    pub fn take_irq_tick(&mut self) -> bool {
        self.ctsi.take_irq_tick()
    }

    #[inline]
    fn watch_check(&mut self, addr: u32, width: u8, v: u32) {
        if self.watch == Some(addr) && self.watch_hits.len() < 64 {
            self.watch_hits.push((self.pc_hint, width, v));
        }
    }

    /// Najczesciej czytane adresy MMIO (do diagnozy busy-poll). Zwraca posortowane malejaco.
    pub fn hot_reads(&self, top: usize) -> Vec<(u32, u64)> {
        let mut v: Vec<_> = self.read_hist.iter().map(|(a, c)| (*a, *c)).collect();
        v.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
        v.truncate(top);
        v
    }

    /// Patch bajtu w KOPII ROM w pamieci (plik na dysku nietkniety). Do eksperymentow
    /// (np. beq->bge w bramce wake 0x299efe, by budzic dormant taski stan 5).
    pub fn patch_rom(&mut self, addr: u32, val: u8) {
        if (ROM_START..ROM_END).contains(&addr) {
            self.rom[(addr - ROM_START) as usize] = val;
        }
    }

    #[inline]
    fn region(addr: u32) -> Region {
        if (ROM_START..ROM_END).contains(&addr) {
            Region::Rom
        } else if (MMIO_START..MMIO_END).contains(&addr) {
            Region::Mmio
        } else if (addr as usize) < RAM_SIZE {
            Region::Ram
        } else {
            Region::Unmapped
        }
    }

    fn record(&mut self, write: bool, width: u8, addr: u32, value: u32) {
        if self.trace.len() < self.trace_limit {
            self.trace.push(Access {
                pc: self.pc_hint,
                write,
                width,
                addr,
                value,
            });
        }
    }

    fn raw_read8(&mut self, addr: u32) -> u8 {
        // SIM_ACCEPT_STATE (env): bramka accept SIMUPL @0x29ed06 czyta byte[0x10fac7] (stan SIM);
        // jesli ==0x65/0x67 -> POST ACCEPT (msg 0x5E1 -> SIM-ready). Runtime nigdy nie osiaga 0x65/0x67
        // -> accept nie pada -> reject. Wymus 0x67 PRZY TYM ODCZYCIE -> bramka accept. Test czy odblokowuje.
        if self.pc_hint == 0x0029_ED06 && addr == 0x0010_FAC7 {
            static SA: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            if dbg_flag(&SA, "SIM_ACCEPT_STATE") {
                eprintln!("[sim_accept_state @tick={} -> wymuszam byte[0x10fac7]=0x67]", self.tick_count);
                return 0x67;
            }
        }
        // SIMDBG="lo:hi" (okno krokow): loguj odczyty RAM przez SERWER SIM (pc 0x299000-0x29B000)
        // -> ujawnia WEJSCIA decyzji accept/reject (niezmienna flaga bramkujaca accept).
        {
            static W: std::sync::OnceLock<Option<(u64, u64)>> = std::sync::OnceLock::new();
            let win = W.get_or_init(|| std::env::var("SIMDBG").ok().and_then(|s| {
                let mut it = s.splitn(2, ':');
                Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
            }));
            if let Some((lo, hi)) = win {
                if self.tick_count >= *lo && self.tick_count <= *hi
                    && (0x0029_9000..0x0029_B000).contains(&self.pc_hint)
                    && addr < 0x0020_0000 {
                    eprintln!("[simrd {addr:#08X}={:02X} @pc={:#08X} tick={}]", self.ram[addr as usize], self.pc_hint, self.tick_count);
                }
            }
        }
        // SIM_GATE (env): CELOWANY hack bramki reject - @0x299966 (`ldrb r0,[r0,0xf]`) wymus bit1=1
        // -> @0x299968 `bhs` pomija kolejkowanie komunikatu-5 (reject). Globalnie WIESZA round-robin,
        // wiec celujemy: tylko w oknie [SIM_GATE_FROM,SIM_GATE_TO] (po PIN) i max SIM_GATE_N razy,
        // by pominac KONKRETNA sesje-reject bez zatrzymania round-robina.
        if self.pc_hint == 0x0029_9966 && addr < 0x0020_0000 {
            static G: std::sync::OnceLock<Option<(u64, u64, u32)>> = std::sync::OnceLock::new();
            let cfg = G.get_or_init(|| {
                if std::env::var("SIM_GATE").is_err() { return None; }
                let from = std::env::var("SIM_GATE_FROM").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
                let to = std::env::var("SIM_GATE_TO").ok().and_then(|s| s.parse().ok()).unwrap_or(u64::MAX);
                let n = std::env::var("SIM_GATE_N").ok().and_then(|s| s.parse().ok()).unwrap_or(u32::MAX);
                Some((from, to, n))
            });
            if let Some((from, to, n)) = *cfg {
                if self.tick_count >= from && self.tick_count <= to && self.sim_gate_cnt < n {
                    self.sim_gate_cnt += 1;
                    if std::env::var("SIM_GATE_LOG").is_ok() {
                        eprintln!("[sim_gate #{} @tick={} byte={:#04X}->bit1]", self.sim_gate_cnt, self.tick_count, self.ram[addr as usize]);
                    }
                    return self.ram[addr as usize] | 0x02;
                }
            }
        }
        // Diagnostyka SIMLOCK: log odczytow EEPROM/PM (0x3D0000-0x400000) z PC. Sprawdz
        // zakres NAJPIERW (tani), flaga env cache'owana (nie co krok).
        if (0x003D_0000..0x0040_0000).contains(&addr) {
            static EE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            if dbg_flag(&EE, "EEPROM_LOG") {
                eprintln!("[ee] read {addr:#08X} @pc={:#08X}", self.pc_hint);
            }
        }
        // DIAGNOSTYKA (env SIM_OK): wymus flage SIM-OK [0x1108D3]=1 - handler MMI 0x273846
        // sprawdza ==1 by POMINAC dialog "SIM-Karte nicht angenommen". Flaga normalnie
        // ustawiana po rejestracji w sieci (baseband/DSP - poza zakresem). Test: czy to
        // jedyna bramka do ekranu glownego.
        // CCONT przez GENSIO (odczyt rejestru).
        if addr == REG_CC_RD {
            return self.ccont.cc_read();
        }
        // Klawiatura: odczyt kolumn (KPD_C).
        if addr == REG_KPD_C {
            return self.keypad.read_c();
        }
        // Test-bypass (env FORCE_ST): flaga self-testu 0x11ff15 - wymus bit6=1 (0x40).
        // Renderer CONTACT (fcn.00244620 case0) sprawdza `bit6==0 -> CONTACT`. Bit6 =
        // "self-test zaliczony". Wymuszenie bit6=1 eliminuje CONTACT SERVICE.
        if self.force_st && addr == 0x0011_FF15 {
            return self.ram[addr as usize] | 0x40;
        }
        // REG_ALL (env): bitmapa dostarczalnosci indykacji 0x110964..0x11096B = wszystkie
        // taski (0xFF). Systemowy root: rejestracja routingu nie zachodzi (nikt nie
        // subskrybuje), wiec indykacje droppowane przez 0x2eac66 -> MMI/eventy osierocone.
        // Wymuszenie wszystkich bitow = "wszyscy zarejestrowani" -> indykacje plyna.
        if self.reg_all && (0x0011_0964..0x0011_096C).contains(&addr) {
            return 0xFF;
        }
        // SELFTEST_SUB (env): wymusza subskrypcje wynikow self-testu w pub/sub, by task 3
        // PUBLIKOWAL wyniki (0x2ebc3c: **0x2ebe0c=0x11ff28 subscriber-flag !=0 && bitmapa
        // *0x2ebe14=0x11ff4c). Dzieki temu self-test NAPRAWDE completuje (licznik->0xf) i
        // jego REALNE completion (nie hack ST_PASS) moze zbootstrapowac kaskade eventow.
        // Test hipotezy: czy realne completion uruchamia kaskade -> MMI rysuje.
        if self.selftest_sub {
            if addr == 0x0011_FF28 {
                return self.ram[addr as usize] | 0x01; // subscriber obecny
            }
            if (0x0011_FF4C..0x0011_FF5C).contains(&addr) {
                return 0xFF; // bitmapa: wszystkie typy wiadomosci zasubskrybowane
            }
        }
        // ST_PASS: bramki CONTACT SERVICE / watchdog shutdown (fcn.002446a0, timer ~501 tick).
        // Watchdog czyta 0x11ff1f: gdy 0 -> licznik 0x11ff1a rosnie, przy >=15 -> SHUTDOWN
        // (self-test FAIL). Niezero = reset licznika (self-test OK). 0x11ff15 bit7 @0x2446ae =
        // sprawdzenie "rysuj CONTACT SERVICE" gdy clear. Wymuszamy pass (telefon nie wylacza sie).
        if self.st_pass {
            if addr == 0x0011_FF1F {
                return self.ram[addr as usize] | 0x01;
            }
            if addr == 0x0011_FF15 && self.pc_hint == 0x0024_46AE {
                return self.ram[addr as usize] | 0x80;
            }
        }
        // SIM_READY (env): wymus flage SIM-ready [0x1108D3]=1 (struct 0x1108CC+7). Ta flaga
        // bramkuje akceptacje SIM: ustawiana przez accept (FUN_002721fc) gdy init OK (msg 0x127).
        // Telefon dostaje 0x128 (reject) -> flaga=0 -> prompt PIN + "SIM nicht angenommen".
        // Wymuszenie =1 ma POMINAC prompt PIN i reject (SIM traktowana jako gotowa) -> standby/menu.
        // SIM_READY_FROM=tick (opcjonalnie): wymuszaj flage=1 DOPIERO od tego kroku - pozwala
        // PIN+faza-2 init przejsc normalnie (prompt widoczny, flaga=0), a potem "zaakceptowac"
        // SIM (flaga=1) by odblokowac standby/menu. Spojny stan vs globalne SIM_READY (limbo).
        if addr == 0x0011_08D3 {
            static SR: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
            let cfg = SR.get_or_init(|| {
                if std::env::var("SIM_READY").is_err() { return None; }
                Some(std::env::var("SIM_READY_FROM").ok().and_then(|s| s.parse().ok()).unwrap_or(0))
            });
            if let Some(from) = *cfg {
                if self.tick_count >= from {
                    return 1;
                }
            }
        }
        // FORCE_B2_AFTER: po N krokach czyść bit2 (0x04) flagi self-testu przy odczycie.
        // Build self-testu kończy ~1.07M (dane ustawione); opóźnione obejście pozwala
        // wyjść z wait-loop 0x2c2630 z poprawnym stanem (vs forsowanie od startu = phantom).
        if self.force_b2_after != 0 && addr == 0x0011_FF15 && self.tick_count > self.force_b2_after {
            return self.ram[addr as usize] & !0x04;
        }
        // FORCE_HW: flaga 0x1106e8 (wynik testu podzespolu 14/15) wymus =1 (OK).
        // Decyduje o testach 14/15 self-testu -> CONTACT. Ustawiana 0 przy init,
        // na 1 tylko gdy podsystem zglosi gotowosc (czego nie emulujemy).
        if self.force_hw && addr == 0x0011_06E8 {
            return 1;
        }
        // FORCE_RDY: flagi gotowosci podsystemow MBUS/SIM sprawdzane w glownej petli
        // bootu (fcn.002ba25c: *0x110FE0 && *0x10B7AE). Init bss=0, ustawiane przez
        // zdarzenia podsystemow ktorych nie emulujemy. Wymus niezero by petla wyszla.
        if self.force_rdy && (addr == 0x0011_0FE0 || addr == 0x0010_B7AE) {
            return 1;
        }
        // MBUS (magistrala szeregowa) — wierny model (CTRL RESET self-clear, STATUS idle).
        if let Some(v) = self.mbus.read(addr) {
            return v;
        }
        // SIM (UART ISO-7816) — rejestry bezczynne (kolejki puste, gotowy).
        if (SIM_BASE..SIM_END).contains(&addr) {
            if let Some(v) = self.sim.read(addr) {
                return v;
            }
        }
        // GENSIO_STATUS (0x2006D): bity RDY (WR=0x01,TR=0x02,RD=0x04) — zawsze gotowy,
        // by `_io_wait(GENSIO_STATUS, ...)` w CCONT/LCD przechodzilo.
        if addr == 0x0002_006D {
            return 0x07;
        }
        // Rejestry CTSI (kontroler przerwan/timer) maja pierwszenstwo.
        if let Some(v) = self.ctsi.read(addr) {
            return v;
        }
        match Self::region(addr) {
            Region::Rom => {
                let off = (addr - ROM_START) as usize;
                // Gdy flash w trybie status/ID (po komendzie erase/program/0x90) -> zwroc
                // status/identyfikator. W trybie Array (normalnie) -> dane z kopii ROM.
                if self.flash.is_array() {
                    self.rom[off]
                } else {
                    self.flash.read_override(off)
                }
            }
            Region::Ram => self.ram[addr as usize],
            Region::Mmio => self.mmio[(addr - MMIO_START) as usize],
            Region::Unmapped => 0xFF,
        }
    }

    fn raw_write8(&mut self, addr: u32, val: u8) {
        if self.wwatch == Some(addr) {
            // Bufor pierscieniowy: trzymaj OSTATNIE 300 zapisow (omija zapelnienie bootem).
            self.wwatch_hits.push((self.pc_hint, val));
            if self.wwatch_hits.len() > 300 {
                self.wwatch_hits.remove(0);
            }
            // WWATCH_LOG: live eprintln (krok=tick_count, pc, val) - dziala w wintest/GUI.
            static WL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            if dbg_flag(&WL, "WWATCH_LOG") {
                eprintln!("[wwatch {addr:#08X}<-{val:#04X} @pc={:#08X} tick={}]", self.pc_hint, self.tick_count);
            }
        }
        // WWRANGE="lo:hi" (hex): loguj zapisy do [lo,hi) z wartoscia != 0xAA (rejestracja handlera
        // = realny adres fn != bss-fill 0xAA). Ujawnia CZY/KTO populuje tablice handlerow SIM.
        {
            static R: std::sync::OnceLock<Option<(u32, u32)>> = std::sync::OnceLock::new();
            let rng = R.get_or_init(|| std::env::var("WWRANGE").ok().and_then(|s| {
                let mut it = s.splitn(2, ':');
                Some((u32::from_str_radix(it.next()?.trim_start_matches("0x"), 16).ok()?,
                      u32::from_str_radix(it.next()?.trim_start_matches("0x"), 16).ok()?))
            }));
            if let Some((lo, hi)) = rng {
                if addr >= *lo && addr < *hi && val != 0xAA {
                    eprintln!("[wwrange {addr:#08X}<-{val:#04X} @pc={:#08X} tick={}]", self.pc_hint, self.tick_count);
                }
            }
        }
        // Eksperyment test-bypass (env FORCE_ST): flaga self-testu 0x11ff15 bit7=OK.
        // Firmware kasuje bit7 gdy test podzespolu (COBBA/GSM - nieemulowany) zawodzi,
        // co daje ekran CONTACT SERVICE. Wymuszamy bit7=1 przy kazdym zapisie flagi,
        // by zobaczyc czy firmware przejdzie ZA CONTACT. (jak "naprawa serwisowa").
        let val = if self.force_st && addr == 0x0011_FF15 { val | 0x80 } else { val };
        // ST_PASS (env): CZYSTY pass self-testu - zachowaj bit6 gdy fail-handler @0x24465e
        // probuje go wyczyscic (flag &= 0xbf). Sub-testy task 3 nie maja subskrybenta pub/sub
        // (0x11ff28=0) -> wyniki droppowane -> timeout 0xDB -> fail. To celowy fix tylko tej
        // jednej instrukcji (vs FORCE_ST globalny, ktory daje patologie checksum 0x2d292c).
        // Pozwala batch2 (MMI) wystartowac CZYSTO by zbadac dalsza sciezke UI.
        let val = if self.st_pass && addr == 0x0011_FF15 && self.pc_hint == 0x0024_465E {
            val | 0x40
        } else {
            val
        };
        // Peryferia wyjsciowe (brzeczyk/wibracja/LED/WDT): obserwuj stan (nie konsumuje).
        self.periph.observe_write(addr, val);
        // DSP: zapis IO_CTSI_DSP (0x20002) bit0 = wlaczenie -> start bootu DSP.
        if addr == REG_CTSI_DSP {
            self.dsp.on_ctsi_dsp_write(val);
        }
        // DSPIF (0x30000): MCU kickuje DSP by przetworzyl komende MDI z mailboxa -> DSP
        // konsumuje (czysci 0x100e0) po opoznieniu. Odblokowuje petle L1 baseband.
        if addr == crate::dsp::REG_DSPIF {
            self.dsp.on_dspif_write(val);
        }
        // Routing LCD (GENSIO -> PCD8544).
        match addr {
            REG_LCD_DATA => {
                if self.tick_count > 75_000_000 && self.tick_count < 77_000_000 {
                    static LP: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
                    if dbg_flag(&LP, "LCD_PC") {
                        eprintln!("[lcd] data @pc={:#08X} tick={}", self.pc_hint, self.tick_count);
                    }
                }
                self.lcd.data(val);
                self.lcd_last_data_pc = self.pc_hint;
            }
            REG_LCD_CMD => {
                self.lcd.command(val);
                self.lcd_last_cmd_pc = self.pc_hint;
            }
            REG_CC_WR => self.ccont.cc_write(val),
            REG_KPD_R => self.keypad.write_r(val),
            REG_DIR_R => self.keypad.write_dir_r(val),
            // IO_CTSI_RST (0x20001): bit 2 = programowy reset CPU (MADos ccont.c:
            // `_io_set_bit(IO_CTSI_RST,0x04); reset()=0x200040`). Firmware ustawia bit
            // i robi `b .` czekajac na restart CPU od entry. Sygnalizujemy reset_request.
            REG_CTSI_RST if val & 0x04 != 0 => self.reset_request = true,
            _ => {}
        }
        // MBUS (CTRL/STATUS/BYTE).
        if self.mbus.write(addr, val) {
            return;
        }
        // SIM (UART ISO-7816).
        if (SIM_BASE..SIM_END).contains(&addr) && self.sim.write(addr, val) {
            return;
        }
        // Rejestry CTSE (kontroler przerwan/timer).
        if self.ctsi.write(addr, val) {
            return;
        }
        match Self::region(addr) {
            Region::Rom => self.rom_writes += 1, // obsluzone wyzej przez flash.write16
            Region::Ram => self.ram[addr as usize] = val,
            Region::Mmio => self.mmio[(addr - MMIO_START) as usize] = val,
            Region::Unmapped => {}
        }
    }

    /// Czy dostep wart zalogowania (MMIO lub poza mapa).
    #[inline]
    fn loggable(addr: u32) -> bool {
        matches!(Self::region(addr), Region::Mmio | Region::Unmapped)
    }

    fn bump_counters(&mut self, addr: u32, write: bool) {
        match Self::region(addr) {
            Region::Mmio if write => self.mmio_writes += 1,
            Region::Mmio => {
                self.mmio_reads += 1;
                *self.read_hist.entry(addr).or_insert(0) += 1;
            }
            Region::Unmapped => self.unmapped += 1,
            _ => {}
        }
    }
}

enum Region {
    Rom,
    Ram,
    Mmio,
    Unmapped,
}

// =================== MemoryInterface (magistrala CPU) ===================

impl MemoryInterface for Machine {
    fn load_8(&mut self, addr: u32, _a: MemoryAccess) -> u8 {
        let v = self.raw_read8(addr);
        if Self::loggable(addr) {
            self.bump_counters(addr, false);
            self.record(false, 8, addr, v as u32);
        }
        self.watch_check(addr, 8, v as u32);
        v
    }

    fn load_16(&mut self, addr: u32, _a: MemoryAccess) -> u16 {
        let a = addr & !1;
        // Firmware DCT3 jest BIG-ENDIAN.
        let v = u16::from_be_bytes([self.raw_read8(a), self.raw_read8(a + 1)]);
        let v = self.dsp.read_fixup(a, v as u32) as u16;
        // DSPRD="lo:hi" (tick): loguj odczyty shared-mem DSP (0x10000-0x10200) - co firmware
        // czyta po MDIRCV (oczekiwany format odpowiedzi L1).
        {
            static D: std::sync::OnceLock<Option<(u64, u64)>> = std::sync::OnceLock::new();
            let win = D.get_or_init(|| std::env::var("DSPRD").ok().and_then(|s| {
                let mut it = s.splitn(2, ':');
                Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
            }));
            if let Some((lo, hi)) = win {
                if self.tick_count >= *lo && self.tick_count <= *hi && (0x0001_0000..0x0001_0200).contains(&a) {
                    eprintln!("[dsprd {a:#08X}={v:#06X} @pc={:#08X} tick={}]", self.pc_hint, self.tick_count);
                }
            }
        }
        if Self::loggable(a) {
            self.bump_counters(a, false);
            self.record(false, 16, a, v as u32);
        }
        self.watch_check(a, 16, v as u32);
        v
    }

    fn load_32(&mut self, addr: u32, _a: MemoryAccess) -> u32 {
        let a = addr & !3;
        // Firmware DCT3 jest BIG-ENDIAN.
        let v = u32::from_be_bytes([
            self.raw_read8(a),
            self.raw_read8(a + 1),
            self.raw_read8(a + 2),
            self.raw_read8(a + 3),
        ]);
        let v = self.dsp.read_fixup(a, v);
        if Self::loggable(a) {
            self.bump_counters(a, false);
            self.record(false, 32, a, v);
        }
        self.watch_check(a, 32, v);
        v
    }

    fn store_8(&mut self, addr: u32, value: u8, _a: MemoryAccess) {
        // Region flash: komenda/dane do FSM flash (na kopii ROM, plik nietkniety).
        if matches!(Self::region(addr), Region::Rom) {
            self.flash_write(addr, value as u16);
            return;
        }
        if Self::loggable(addr) {
            self.bump_counters(addr, true);
            self.record(true, 8, addr, value as u32);
        }
        self.raw_write8(addr, value);
    }

    fn store_16(&mut self, addr: u32, value: u16, _a: MemoryAccess) {
        let a = addr & !1;
        // Region flash: komenda/dane 16-bit do FSM flash.
        if matches!(Self::region(a), Region::Rom) {
            self.flash_write(a, value);
            return;
        }
        if Self::loggable(a) {
            self.bump_counters(a, true);
            self.record(true, 16, a, value as u32);
        }
        if vwatch_val() == Some(value as u32) { self.wwatch_hits.push((self.pc_hint, 0)); }
        let b = value.to_be_bytes(); // BIG-ENDIAN
        self.raw_write8(a, b[0]);
        self.raw_write8(a + 1, b[1]);
    }

    fn store_32(&mut self, addr: u32, value: u32, _a: MemoryAccess) {
        let a = addr & !3;
        // Region flash: dwa slowa 16-bit do FSM flash (BIG-ENDIAN).
        if matches!(Self::region(a), Region::Rom) {
            self.flash_write(a, (value >> 16) as u16);
            self.flash_write(a + 2, value as u16);
            return;
        }
        if Self::loggable(a) {
            self.bump_counters(a, true);
            self.record(true, 32, a, value);
        }
        if vwatch_val() == Some(value) { self.wwatch_hits.push((self.pc_hint, 0)); }
        let b = value.to_be_bytes(); // BIG-ENDIAN
        for (i, byte) in b.iter().enumerate() {
            self.raw_write8(a + i as u32, *byte);
        }
    }

    fn idle_cycle(&mut self) {}
}

// =================== BusIO / DebugRead (bez logowania) ===================

impl BusIO for Machine {
    fn read_8(&mut self, addr: Addr) -> u8 {
        self.raw_read8(addr)
    }
    fn write_8(&mut self, addr: Addr, value: u8) {
        self.raw_write8(addr, value);
    }
}

impl DebugRead for Machine {
    fn debug_read_8(&mut self, addr: Addr) -> u8 {
        self.raw_read8(addr)
    }
}
