//! Model interfejsu karty SIM (UART ISO-7816) — wg MADos hw/sim.c.
//!
//! Rejestry (baza 0x20000): TXD=0x36, RXD=0x37, UART_INT=0x38, CTRL=0x39,
//!   CLK_CTRL=0x3A, TXD_LWM=0x3B, RXD_QUE=0x3C, RXD_FL=0x3D, TXD_FL=0x3E, TXD_QUE=0x3F.
//! Przerwania: FIQ_SIMUART=6, FIQ_SIMCARDDETX=7 (detekcja karty).
//!
//! Stan obecny: karta OBECNA (card_present), ale logika ATR/APDU nieaktywna dopóki
//! firmware nie dojdzie do init SIM (obecnie parkuje się wcześniej). Rejestry zwracają
//! wartości bezczynne: kolejki RXD/TXD puste, brak przerwania UART — żeby nie podać
//! firmware'owi śmieci (domyślne 0xFF z mmio[] = np. RXD_QUE=255 bajtów = błąd).

pub const SIM_BASE: u32 = 0x0002_0036;
pub const SIM_END: u32 = 0x0002_0040; // 0x36..0x40 (10 rejestrów)

const REG_TXD: u32 = 0x0002_0036;
const REG_RXD: u32 = 0x0002_0037;
const REG_UART_INT: u32 = 0x0002_0038;
const REG_CTRL: u32 = 0x0002_0039;
const REG_CLK_CTRL: u32 = 0x0002_003A;
const REG_TXD_LWM: u32 = 0x0002_003B;
const REG_RXD_QUE: u32 = 0x0002_003C;
const REG_RXD_FL: u32 = 0x0002_003D;
const REG_TXD_FL: u32 = 0x0002_003E;
const REG_TXD_QUE: u32 = 0x0002_003F;

pub struct Sim {
    /// Czy karta jest fizycznie obecna (linia card-detect).
    pub card_present: bool,
    ctrl: u8,
    clk_ctrl: u8,
    txd_fl: u8,
    rxd_fl: u8,
    txd_lwm: u8,
    /// Bufor odpowiedzi karty (ATR/APDU) do wystawienia na RXD.
    rx_queue: std::collections::VecDeque<u8>,
    /// SIM aktywowana (firmware ustawil CTRL bit7 = RST high) -> wyslij ATR.
    activated: bool,
    /// Odliczanie do wystawienia kolejnego bajtu RX (modeluje baud SIM ~slow).
    rx_delay: u32,
    /// Bajty TXD od telefonu (komenda APDU) - akumulowane do parsowania.
    pub tx_bytes: Vec<u8>,
    /// Bufor zbieranej komendy TPDU (T=0): CLA INS P1 P2 P3 [+dane].
    apdu: Vec<u8>,
    /// Case-3: ile bajtow danych jeszcze oczekiwanych po naglowku (telefon -> karta).
    data_expected: usize,
    /// Przygotowany bufor GET RESPONSE (FCP z ostatniego SELECT).
    gr: Vec<u8>,
    /// Aktualnie wybrany plik (file ID) — do READ/GET RESPONSE.
    selected: u16,
    /// Bieżący katalog DF/MF (file ID) — do STATUS. GSM 11.11: STATUS zwraca FCP
    /// bieżącego KATALOGU, nie wybranego EF. Aktualizowany przy SELECT DF/MF.
    current_df: u16,
    /// Odliczanie do przerwania TX-ready (FIFO TX oprozniony po zapisach). Re-arm na
    /// kazdy zapis TXD; po ostatnim -> tx_pending. Bez tego firmware nie dosyla komendy.
    tx_ready_delay: u32,
    /// Czekajace przerwanie TX-ready (UART_INT bit4) - ISR 0x2d3f14 kontynuuje TX.
    tx_pending: bool,
}

/// ATR (Answer To Reset) minimalnej karty GSM 2G, T=0, direct convention.
/// TS=0x3B, T0=0x15 (TA1 obecny, K=5 historycznych), TA1=0x18, 5 historycznych.
/// 8 bajtow = TS+T0+TA1+5hist. Dobrze uformowane (firmware czyta dokladnie 8, bez timeoutu).
const ATR: &[u8] = &[
    0x3B, 0x15, 0x18, 0x00, 0x80, 0x31, 0x80, 0x65,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00,
];
/// Krokow CPU miedzy bajtami RX (baud SIM). ~1 etu @ kilka kHz.
const RX_BYTE_DELAY: u32 = 30;
/// Krokow CPU do przerwania TX-ready po ostatnim zapisie TXD (FIFO oprozniony).
/// Mniejsze niz RX_BYTE_DELAY by TX-ready padlo przed RX-ready odpowiedzi.
const TX_READY_DELAY: u32 = 15;

/// Czy komenda GSM jest case-3 (telefon -> karta dane po naglowku): SELECT/VERIFY/
/// CHANGE/UNBLOCK CHV, UPDATE BINARY/RECORD, INCREASE, TERMINAL PROFILE (0x10, SIM Toolkit:
/// telefon wysyla profil + SW), TERMINAL RESPONSE (0x14). Inne = case-2 (karta -> telefon).
fn is_case3(ins: u8) -> bool {
    matches!(ins, 0xA4 | 0x20 | 0x24 | 0x2C | 0xD6 | 0xDC | 0x32 | 0x10 | 0x14)
}

/// Buduje odpowiedz SELECT (FCP wg GSM 11.11 / TS 51.011) dla danego file ID.
/// Format wzorowany na swsim (.vendor/swsim src/gsm.c gsm_select_res).
/// MF (0x3F..)/DF (0x7F..) = 22 bajty (część obowiązkowa); EF = 15 bajtow.
fn gsm_select_response(fid: u16) -> Vec<u8> {
    let hi = (fid >> 8) as u8;
    if hi == 0x3F || hi == 0x7F {
        // MF (0x3F00) lub DF (0x7Fxx). 22 bajtow (indeksy 0-21, część obowiązkowa).
        let ftype = if hi == 0x3F { 0x01 } else { 0x02 };
        let (df_cnt, ef_cnt) = if hi == 0x3F { (0x02, 0x01) } else { (0x00, 0x10) };
        let mut r = vec![0u8; 22];
        r[2] = 0xFF; // bajt 3-4: wolna pamiec
        r[3] = 0xFF;
        r[4] = (fid >> 8) as u8; // bajt 5-6: file ID
        r[5] = fid as u8;
        r[6] = ftype; // bajt 7: typ (01=MF, 02=DF)
        r[12] = 0x0A; // bajt 13: dlugosc danych GSM (10)
        // bajt 14: char pliku 0b10110010 = CHV1 disabled (bit7) + clock/napiecie
        r[13] = 0xB2;
        r[14] = df_cnt; // bajt 15: liczba DF (dzieci)
        r[15] = ef_cnt; // bajt 16: liczba EF (dzieci)
        r[16] = 0x04; // bajt 17: liczba CHV/UNBLOCK/admin (4)
        r[18] = 0x83; // bajt 19: status CHV1 (zainicjalizowany, 3 proby)
        r[19] = 0x8A; // bajt 20: status UNBLOCK CHV1
        r[20] = 0x83; // bajt 21: status CHV2
        r[21] = 0x8A; // bajt 22: status UNBLOCK CHV2
        r
    } else {
        // EF (0x6Fxx, 0x2Fxx, 0x4Fxx...). 15 bajtow.
        // Struktura: pliki REKORDOWE (MSISDN/ADN/LND/ext) musza zwracac linear-fixed(01)/
        // cyclic(03) + dlugosc rekordu - inaczej telefon SELECTuje ale nie czyta i init utyka.
        let (structure, rec_len) = match fid {
            0x6F40 | 0x6F3A | 0x6F3B | 0x6F49 | 0x6F4A | 0x6F4B | 0x6F4C => (0x01u8, 0x1Cu8),
            0x6F44 | 0x6F4D => (0x03u8, 0x1Cu8), // cyclic (LND/...)
            _ => (0x00u8, 0x00u8),               // transparent
        };
        let size: u16 = if rec_len > 0 { rec_len as u16 * 4 } else { 0x20 };
        let mut r = vec![0u8; 15];
        r[2] = (size >> 8) as u8; // bajt 3-4: rozmiar pliku
        r[3] = size as u8;
        r[4] = (fid >> 8) as u8; // bajt 5-6: file ID
        r[5] = fid as u8;
        r[6] = 0x04; // bajt 7: EF
        r[7] = 0x00; // bajt 8: b7 (0=nie cyclic)
        // bajt 9-11: warunki dostepu (00 = READ ALWAYS)
        r[11] = 0x01; // bajt 12: status pliku (aktywny/nie uniewazniony)
        r[12] = 0x02; // bajt 13: dlugosc nastepujacych
        r[13] = structure; // bajt 14: struktura (00=transp, 01=linear-fixed, 03=cyclic)
        r[14] = rec_len; // bajt 15: dlugosc rekordu
        r
    }
}

/// Tresc znanych plikow EF (GSM 11.11, wartosci wzorowane na swsim data/gsm.json).
/// Krytyczne dla bootu: EF_phase (6FAE)=0x03 Phase2+, EF_SST (6F38) tablica uslug.
/// Nieznane EF -> pusta (READ zwroci 0xFF pad).
fn ef_content(fid: u16) -> Vec<u8> {
    match fid {
        // ICCID (2FE2) - numer karty (BCD, swap nibbli)
        0x2FE2 => vec![0x98, 0x99, 0x99, 0x90, 0x00, 0x00, 0x00, 0x00, 0x00, 0xF1],
        // IMSI (6F07) = 262011234567890 (MCC 262 DE, MNC 01) - poprawny format BCD swap,
        // parity odd (15 cyfr -> low nibble bajtu 2 = 0x9). Testowy MCC 999 byl odrzucany.
        0x6F07 => vec![0x08, 0x29, 0x26, 0x10, 0x21, 0x43, 0x65, 0x87, 0x09],
        // SIM Service Table (6F38) - ktore uslugi dostepne/aktywowane
        0x6F38 => vec![
            0xFF, 0x3F, 0xFF, 0xFF, 0x3F, 0x00, 0x3F, 0x0F, 0x30, 0x0C, 0x00, 0x00, 0x00, 0xC0,
        ],
        // Access Control Class (6F78) - 16 bitow klas 0-15; SIM ma JEDNA klase (0xFFFF=zle).
        // Klasa 2: bajt2 bit2 = 0x04. Puste (0xFF 0xFF) moglo dawac "SIM nicht angenommen".
        0x6F78 => vec![0x00, 0x04],
        // Administrative Data (6FAD) - tryb operacji + dlugosc MNC. bajt4 = dlugosc MNC (2).
        0x6FAD => vec![0x00, 0x00, 0x00, 0x02],
        // Phase (6FAE) = 0x03 (Phase 2+). KRYTYCZNE - 0xFF zablokowaloby init.
        0x6FAE => vec![0x03],
        // Preferred Languages (6F05) - kody jezykow
        0x6F05 => vec![0x00, 0x01, 0x02, 0x03],
        _ => Vec::new(),
    }
}

impl Default for Sim {
    fn default() -> Self {
        Self::new()
    }
}

impl Sim {
    pub fn new() -> Self {
        Self {
            card_present: true,
            ctrl: 0,
            clk_ctrl: 0,
            txd_fl: 0,
            rxd_fl: 0,
            txd_lwm: 0,
            rx_queue: std::collections::VecDeque::new(),
            activated: false,
            rx_delay: 0,
            tx_bytes: Vec::new(),
            apdu: Vec::new(),
            data_expected: 0,
            gr: Vec::new(),
            selected: 0x3F00,
            current_df: 0x3F00,
            tx_ready_delay: 0,
            tx_pending: false,
        }
    }

    /// Dolaczenie bajtow odpowiedzi karty do kolejki RX (uzbraja FIQ bit6 jesli pusto).
    fn queue_resp(&mut self, bytes: &[u8]) {
        let was_empty = self.rx_queue.is_empty();
        self.rx_queue.extend(bytes.iter().copied());
        if was_empty && !self.rx_queue.is_empty() {
            self.rx_delay = RX_BYTE_DELAY;
        }
    }

    /// Maszyna stanow T=0 (BEZ procedure byte - to FW nie uzywa). Wolana po kazdym
    /// bajcie TXD. Naglowek = 5 bajtow (CLA INS P1 P2 P3). FW wysyla cala komende
    /// (header [+dane case-3]) napedzany przerwaniem TX-ready, potem czeka na odpowiedz.
    /// Case-3 (telefon -> karta: SELECT/VERIFY/UPDATE): czekaj P3 bajtow danych, potem SW.
    /// Case-2 (karta -> telefon: STATUS/GET RESPONSE/READ): odeslij P3 bajtow danych + SW.
    fn apdu_step(&mut self) {
        if self.data_expected == 0 && self.apdu.len() == 5 {
            let ins = self.apdu[1];
            let p3 = self.apdu[4] as usize;
            if std::env::var("SIM_INS").is_ok() {
                eprintln!("[ins] CLA={:#04x} INS={:#04x} P1={:#04x} P2={:#04x} P3={} case3={}",
                    self.apdu[0], ins, self.apdu[2], self.apdu[3], p3, is_case3(ins));
            }
            if ins == 0x20 && std::env::var("SIM_LOG").is_ok() {
                eprintln!("[sim] VERIFY CHV (PIN OK!) P1={:#04x} P2={:#04x} P3={}", self.apdu[2], self.apdu[3], p3);
            }
            if is_case3(ins) && p3 > 0 {
                self.queue_resp(&[ins]); // procedure byte ACK -> telefon dosyla dane (file ID)
                self.data_expected = p3; // czekaj az telefon dosle dane
            } else if is_case3(ins) {
                self.respond_case3(); // P3==0: brak danych, od razu SW
                self.apdu.clear();
            } else {
                // case-2: karta wysyla procedure byte (INS=ACK) + dane + SW
                let data = self.case2_data(ins, p3);
                self.queue_resp(&[ins]); // procedure byte ACK (jak case-3)
                self.queue_resp(&data);
                self.queue_resp(&[0x90, 0x00]);
                self.apdu.clear();
            }
        } else if self.data_expected > 0 && self.apdu.len() == 5 + self.data_expected {
            self.respond_case3(); // case-3 kompletny: wszystkie dane odebrane
            self.apdu.clear();
            self.data_expected = 0;
        }
    }

    /// Odpowiedz po komendzie case-3 (telefon doslal dane). SELECT -> 9F len (GET RESPONSE);
    /// inne (VERIFY/UPDATE) -> 90 00.
    fn respond_case3(&mut self) {
        let ins = self.apdu[1];
        if ins == 0xA4 {
            let fid = ((self.apdu[5] as u16) << 8) | self.apdu[6] as u16;
            if std::env::var("SIM_LOG").is_ok() {
                eprintln!("[sim] SELECT {fid:#06x}");
            }
            // Pliki NIEISTNIEJACE na standardowej karcie GSM -> "file not found" (SW 94 04),
            // by telefon je POMINAL zamiast czekac. 7F40 = DF niestandardowy (Nokia/operator);
            // fake-success powodowal zatrzymanie init po SELECT 7F40. Z not-found init RUSZA
            // dalej -> "PIN OK" (akceptacja). Poprawne modelowanie SIM, nie hack.
            if fid == 0x7F40 {
                self.queue_resp(&[0x94, 0x04]);
                return;
            }
            self.selected = fid;
            let hi = (fid >> 8) as u8;
            if hi == 0x3F || hi == 0x7F { self.current_df = fid; } // SELECT katalogu -> bieżący DF
            self.gr = gsm_select_response(fid);
            self.queue_resp(&[0x9F, self.gr.len() as u8]); // dane dostepne -> GET RESPONSE
        } else {
            self.queue_resp(&[0x90, 0x00]); // sukces
        }
    }

    /// Dane dla komend case-2 (karta -> telefon), P3 bajtow.
    fn case2_data(&self, ins: u8, p3: usize) -> Vec<u8> {
        let src = match ins {
            0xC0 => self.gr.clone(),                    // GET RESPONSE: FCP z SELECT
            0xF2 => gsm_select_response(self.current_df), // STATUS: FCP biezacego KATALOGU (nie EF!)
            0xB0 | 0xB2 => {
                // READ BINARY/RECORD: tresc wybranego EF od offsetu P1P2.
                let off = ((self.apdu[2] as usize) << 8) | self.apdu[3] as usize;
                if std::env::var("SIM_LOG").is_ok() {
                    eprintln!("[sim] READ {:#06x} off={off} len={p3}", self.selected);
                }
                ef_content(self.selected).into_iter().skip(off).collect()
            }
            _ => Vec::new(),
        };
        let mut out = vec![0xFFu8; p3];
        for (i, b) in src.iter().take(p3).enumerate() {
            out[i] = *b;
        }
        out
    }

    /// Tik RX (wolany co krok CPU). Zwraca true gdy bajt RX jest gotowy do odczytu
    /// -> magistrala asertuje FIQ bit6 (SIMUART). Bajt zostaje na czele rx_queue do
    /// odczytu przez RXD (ISR). Modeluje baud SIM (RX_BYTE_DELAY krokow miedzy bajtami).
    pub fn rx_tick(&mut self) -> bool {
        // TX-ready: po zapisach TXD (telefon przestal pisac = FIFO oprozniony) asertuj
        // przerwanie TX-ready (UART_INT bit4) -> ISR 0x2d3f14 kontynuuje/konczy TX komendy.
        if self.tx_ready_delay > 0 {
            self.tx_ready_delay -= 1;
            if self.tx_ready_delay == 0 {
                self.tx_pending = true;
                return true; // assert FIQ bit6 (SIMUART) - zrodlo TX-ready
            }
        }
        // RX-ready: kolejny bajt odpowiedzi karty gotowy.
        if self.rx_queue.is_empty() {
            return false;
        }
        if self.rx_delay > 0 {
            self.rx_delay -= 1;
            return false;
        }
        // Puls (nie storm): po asercji re-arm opoznienie. FIQ bit6 pada raz na
        // RX_BYTE_DELAY krokow dla bajtu na czele kolejki (ISR czyta -> nastepny).
        self.rx_delay = RX_BYTE_DELAY;
        true // bajt gotowy -> assert FIQ bit6 (jeden puls)
    }

    /// Odczyt rejestru SIM; None => nie nasz adres.
    pub fn read(&mut self, addr: u32) -> Option<u8> {
        Some(match addr {
            // RXD: kolejny bajt z kolejki odbiorczej (0 gdy pusto). Po odczycie ustaw
            // opoznienie do nastepnego bajtu (baud) - FIQ bit6 zaasertuje sie ponownie.
            REG_RXD => {
                let b = self.rx_queue.pop_front().unwrap_or(0);
                if !self.rx_queue.is_empty() {
                    self.rx_delay = RX_BYTE_DELAY;
                }
                b
            }
            // UART_INT: status przerwania UART. ISR SIM (0x2d4140) bada bity:
            // bit6 = RX-ready -> handler RX 0x2d40b8 czyta RXD_QUE+RXD (drenuje FIFO).
            // Ustawiamy bit6 gdy kolejka RX niepusta (bajt ATR/APDU gotowy).
            REG_UART_INT => {
                let mut v = 0u8;
                if !self.rx_queue.is_empty() {
                    v |= 0x40; // bit6 = RX-ready
                }
                if self.tx_pending {
                    v |= 0x10; // bit4 = TX-ready (FIFO oprozniony)
                }
                v
            }
            // CTRL: bity sterujace pisze firmware (bit0=power, bit7=RST). bit6 (0x40) to
            // STATUS SPRZETOWY "karta aktywna/zegar stabilny, gotowa do TX" - firmware
            // sprawdza go (fcn 0x2d3eba) przed wyslaniem APDU. Po aktywacji (RST high =
            // ATR) karta jest zaclockowana -> bit6 czyta sie jako ustawiony.
            REG_CTRL => self.ctrl | if self.activated { 0x40 } else { 0 },
            REG_CLK_CTRL => self.clk_ctrl,
            REG_TXD_LWM => self.txd_lwm,
            // RXD_QUE: liczba bajtów w kolejce odbiorczej.
            REG_RXD_QUE => self.rx_queue.len().min(0xFF) as u8,
            REG_RXD_FL => self.rxd_fl,
            REG_TXD_FL => self.txd_fl,
            // TXD_QUE: liczba bajtów w kolejce nadawczej — pusto (gotowy do nadawania).
            REG_TXD_QUE => 0x00,
            REG_TXD => 0x00,
            _ => return None,
        })
    }

    /// Zapis rejestru SIM; true => obsłużono.
    pub fn write(&mut self, addr: u32, val: u8) -> bool {
        match addr {
            REG_TXD => {
                self.tx_bytes.push(val); // bajt komendy APDU od telefonu
                if self.activated && std::env::var("SIM_ATR").is_ok() {
                    // Komenda GSM TPDU zaczyna sie CLA=0xA0; bajty idle/guard (FF/00)
                    // miedzy komendami pomijamy, by nie tworzyc smieciowych komend.
                    if !self.apdu.is_empty() || val == 0xA0 {
                        self.apdu.push(val);
                        self.apdu_step();
                    }
                    // Zapis TXD -> FIFO sie zapelnia; po ostatnim zapisie (re-arm) FIFO
                    // sie oproznia -> przerwanie TX-ready. Re-arm na kazdy bajt.
                    self.tx_ready_delay = TX_READY_DELAY;
                }
            }
            // write-1-clear przerwań UART: bit4 (TX-ready) kasuje tx_pending.
            REG_UART_INT => {
                if val & 0x10 != 0 {
                    self.tx_pending = false;
                }
            }
            REG_CTRL => {
                // CTRL bit7 (0x80) = RST high -> aktywacja karty -> wyslij ATR.
                // Wykrycie zbocza: bit7 ustawiony i wczesniej nie aktywowana.
                // ATR domyslnie WYLACZONE (env SIM_ATR=1) - bez tego model ATR robi
                // FIQ-storm (ISR nie opróżnia kolejki) i psuje ekran "Wloz SIM".
                if val & 0x80 != 0 && !self.activated && self.card_present
                    && std::env::var("SIM_ATR").is_ok()
                {
                    self.activated = true;
                    self.rx_queue.clear();
                    self.rx_queue.extend(ATR.iter().copied());
                    self.rx_delay = RX_BYTE_DELAY;
                }
                // RST low (bit7=0) deaktywuje (kolejny reset wysle ATR ponownie).
                if val & 0x80 == 0 {
                    self.activated = false;
                }
                self.ctrl = val;
            }
            REG_CLK_CTRL => self.clk_ctrl = val,
            REG_TXD_LWM => self.txd_lwm = val,
            REG_RXD_FL => self.rxd_fl = val,
            REG_TXD_FL => self.txd_fl = val,
            _ if (SIM_BASE..SIM_END).contains(&addr) => {}
            _ => return false,
        }
        true
    }
}
