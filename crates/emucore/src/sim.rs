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
    /// Aktualnie wybrany plik (file ID).
    selected: u16,
    /// Odliczanie do przerwania TX-ready (FIFO TX oprozniony po zapisach). Re-arm na
    /// kazdy zapis TXD; po ostatnim -> tx_pending. Bez tego firmware nie dosyla komendy.
    tx_ready_delay: u32,
    /// Czekajace przerwanie TX-ready (UART_INT bit4) - ISR 0x2d3f14 kontynuuje TX.
    tx_pending: bool,
    /// Mutowalny store tresci EF (UPDATE pisze tu, READ czyta stad zamiast statycznego
    /// ef_content). Inicjowany z default_ef_data(). Pozwala na UPDATE BINARY/RECORD (D6/DC).
    ef_data: std::collections::HashMap<u16, Vec<u8>>,
    /// Ki (klucz uwierzytelniajacy) do RUN GSM ALGORITHM (A3/A8). 16 bajtow.
    ki: [u8; 16],
    /// Czy CHV1 (PIN) zweryfikowany w tej sesji (po VERIFY 90 00).
    chv1_verified: bool,
}

/// Domyslna tresc plikow EF (HashMap, do mutowalnego store). Pliki czytane przy boocie
/// (2FE2/6F05/6F07/6F38/6F78/6FAD/6FAE/6F14/6F3E/6F3F/6F46/6FB7) maja wartosci jak w
/// ef_content() (boot STABILNY z tymi wartosciami). Pliki sieciowe (6F7E LOCI/6F30/6F20...)
/// dodane wg swsim gsm.json - telefon je czyta gdy idzie dalej do rejestracji.
fn default_ef_data() -> std::collections::HashMap<u16, Vec<u8>> {
    let mut m = std::collections::HashMap::new();
    // Pliki boot (zachowane z ef_content - boot stabilny).
    for fid in [0x2FE2u16, 0x6F07, 0x6F38, 0x6F78, 0x6FAD, 0x6FAE, 0x6F05] {
        m.insert(fid, ef_content(fid));
    }
    // 6F7E LOCI (Location Information, 11B): TMSI[4]=FFFFFFFF + LAI[5] (MCC262 MNC01,
    // LAC FFFE) + TMSI_TIME[1]=FF + LUS[1]=00 (updated). Telefon czyta przy rejestracji.
    m.insert(0x6F7E, vec![0xFF, 0xFF, 0xFF, 0xFF, 0x62, 0xF2, 0x10, 0xFF, 0xFE, 0xFF, 0x00]);
    // 6F46 SPN (Service Provider Name, 17B): byte0=display cond(0x01), reszta nazwa ASCII pad 0xFF.
    m.insert(0x6F46, {
        let mut v = vec![0xFFu8; 17]; v[0] = 0x01;
        for (i, c) in b"Emu".iter().enumerate() { v[1 + i] = *c; }
        v
    });
    // 6F30 PLMNsel (preferowane sieci) / 6F20 Kc / 6F31 HPLMN / 6F37 ACMmax / 6F39 ACM /
    // 6F41 PUCT / 6F45 CBMI / 6F74 BCCH / 6F7B FPLMN: puste (0xFF) - poprawne "nie ustawione".
    for fid in [0x6F30u16, 0x6F20, 0x6F31, 0x6F37, 0x6F39, 0x6F41, 0x6F45, 0x6F74, 0x6F7B] {
        m.insert(fid, vec![0xFF; 16]);
    }
    m
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
    // 0x88 RUN GSM ALGORITHM (telefon -> RAND 16B), 0xC2 ENVELOPE (Toolkit) tez case-3.
    matches!(ins, 0xA4 | 0x20 | 0x24 | 0x2C | 0xD6 | 0xDC | 0x32 | 0x10 | 0x14 | 0x88 | 0xC2)
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
        // SIM_STRICT: swsim zwraca 23B (indeks 22 = 0x00 admin). RYZYKO: testowane=destabilizuje.
        if std::env::var("SIM_STRICT").is_ok() {
            r.push(0x00);
        }
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
        // SIM_STRICT: swsim ZAWSZE rec_len=0 (nawet rekordowe). RYZYKO: psuje indeksowanie B2.
        r[14] = if std::env::var("SIM_STRICT").is_ok() { 0 } else { rec_len };
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

/// Czy plik istnieje w drzewie GSM (wg swsim gsm.json). Uzywane tylko w SIM_STRICT.
/// MF/DF + EF pod TELECOM/GSM. UWAGA: boot czyta 6F14/6FB7 spoza tej listy.
fn file_exists(fid: u16) -> bool {
    matches!(fid,
        0x3F00 | 0x2FE2 | 0x7F10 | 0x7F20 |
        // DF_TELECOM (7F10)
        0x6F3A | 0x6F3B | 0x6F3C | 0x6F40 | 0x6F42 | 0x6F43 | 0x6F44 |
        0x6F47 | 0x6F49 | 0x6F4A | 0x6F4B | 0x6F4C |
        // DF_GSM (7F20)
        0x6F05 | 0x6F07 | 0x6F20 | 0x6F30 | 0x6F31 | 0x6F37 | 0x6F38 | 0x6F39 |
        0x6F3E | 0x6F3F | 0x6F41 | 0x6F45 | 0x6F46 | 0x6F74 | 0x6F78 | 0x6F7B |
        0x6F7E | 0x6FAD | 0x6FAE
    )
}

/// GSM A3/A8 (RUN GSM ALGORITHM): RAND(16) + Ki -> SRES(4) + Kc(8) = 12 bajtow.
/// Bez prawdziwego COMP128 (Ki=0 testowe) - deterministyczna namiastka (XOR/rotacja),
/// wystarczy by telefon dostal 12B odpowiedzi 9F 0C. Sieci i tak nie emulujemy.
fn gsm_a3a8(ki: &[u8; 16], rand: &[u8; 16]) -> Vec<u8> {
    let mut mix = [0u8; 16];
    for i in 0..16 {
        mix[i] = rand[i] ^ ki[i] ^ rand[(i + 1) & 15];
    }
    let mut out = Vec::with_capacity(12);
    out.extend_from_slice(&mix[0..4]);  // SRES
    out.extend_from_slice(&mix[4..12]); // Kc
    out
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
            tx_ready_delay: 0,
            tx_pending: false,
            ef_data: default_ef_data(),
            ki: [0x00; 16],
            chv1_verified: false,
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
                // SIM_STRICT: walidacja READ wg swsim (94 08 nie-transparent / 67 00 zla dlugosc).
                if std::env::var("SIM_STRICT").is_ok() {
                    if let Some(sw) = self.case2_strict_sw(ins, p3) {
                        self.queue_resp(&sw);
                        self.apdu.clear();
                        return;
                    }
                }
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
        match ins {
            0xA4 => {
                let fid = ((self.apdu[5] as u16) << 8) | self.apdu[6] as u16;
                if std::env::var("SIM_LOG").is_ok() {
                    eprintln!("[sim] SELECT {fid:#06x}");
                }
                // SIM_STRICT: walidacja wg swsim apduh_gsm_select. P1=P2=0, P3=2 -> 6B 00;
                // plik nieistniejacy -> 94 04. RYZYKO: boot czyta 6F14/6FB7 (poza drzewem swsim)
                // -> strict je odrzuci i moze zdestabilizowac boot. Domyslnie WYLACZONE.
                if std::env::var("SIM_STRICT").is_ok() {
                    if self.apdu[2] != 0 || self.apdu[3] != 0 || self.apdu[4] != 2 {
                        self.queue_resp(&[0x6B, 0x00]);
                        return;
                    }
                    if !file_exists(fid) {
                        self.queue_resp(&[0x94, 0x04]); // file not found
                        return;
                    }
                }
                self.selected = fid;
                self.gr = gsm_select_response(fid);
                self.queue_resp(&[0x9F, self.gr.len() as u8]); // dane dostepne -> GET RESPONSE
            }
            0x88 => {
                // RUN GSM ALGORITHM: dane = RAND(16). Wynik SRES(4)+Kc(8)=12B przez GET RESPONSE.
                let rand: [u8; 16] = self.apdu.get(5..21)
                    .map(|s| s.try_into().unwrap_or([0u8; 16]))
                    .unwrap_or([0u8; 16]);
                self.gr = gsm_a3a8(&self.ki, &rand);
                if std::env::var("SIM_LOG").is_ok() {
                    eprintln!("[sim] RUN GSM ALGO -> SRES+Kc {} B", self.gr.len());
                }
                self.queue_resp(&[0x9F, self.gr.len() as u8]); // 9F 0C -> GET RESPONSE 12B
            }
            0xD6 | 0xDC => {
                // UPDATE BINARY (D6, offset P1P2) / UPDATE RECORD (DC, nr=P1). Zapis do ef_data.
                let data: Vec<u8> = self.apdu.get(5..).map(|s| s.to_vec()).unwrap_or_default();
                let off = if ins == 0xD6 {
                    ((self.apdu[2] as usize) << 8) | self.apdu[3] as usize
                } else {
                    // RECORD: P1 = nr rekordu (1-based) * rec_len. rec_len z FCP.
                    let rl = self.rec_len(self.selected);
                    (self.apdu[2] as usize).saturating_sub(1) * rl
                };
                let buf = self.ef_data.entry(self.selected).or_default();
                if off + data.len() > buf.len() {
                    buf.resize(off + data.len(), 0xFF);
                }
                buf[off..off + data.len()].copy_from_slice(&data);
                if std::env::var("SIM_LOG").is_ok() {
                    eprintln!("[sim] UPDATE {:#06x} off={off} len={}", self.selected, data.len());
                }
                self.queue_resp(&[0x90, 0x00]);
            }
            0x20 => {
                // VERIFY CHV: P2 = nr CHV. Dane = 8B PIN. Akceptuj (CHV1 disabled w FCP).
                if std::env::var("SIM_STRICT").is_ok() {
                    if self.apdu[2] != 0 { self.queue_resp(&[0x6A, 0x86]); return; }
                    let p3 = self.apdu[4];
                    if p3 == 0x00 { self.queue_resp(&[0x63, 0xC3]); return; } // zapytaj o proby
                    if p3 != 0x08 { self.queue_resp(&[0x67, 0x00]); return; }
                }
                self.chv1_verified = true;
                if std::env::var("SIM_LOG").is_ok() {
                    eprintln!("[sim] VERIFY CHV P2={:#04x} -> OK", self.apdu[3]);
                }
                self.queue_resp(&[0x90, 0x00]);
            }
            0x24 => {
                // CHANGE CHV: dane = 8B stary + 8B nowy. Akceptuj.
                self.queue_resp(&[0x90, 0x00]);
            }
            0x2C => {
                // UNBLOCK CHV: dane = 8B PUK + 8B nowy PIN. Akceptuj.
                self.chv1_verified = true;
                self.queue_resp(&[0x90, 0x00]);
            }
            0xC2 => {
                // ENVELOPE (SIM Toolkit): brak proactive -> 90 00 (lub 9F xx z odpowiedzia).
                self.queue_resp(&[0x90, 0x00]);
            }
            _ => {
                self.queue_resp(&[0x90, 0x00]); // INCREASE/TERMINAL PROFILE/TERMINAL RESPONSE: sukces
            }
        }
    }

    /// Dlugosc rekordu wybranego pliku (z FCP) - do indeksowania READ/UPDATE RECORD.
    fn rec_len(&self, fid: u16) -> usize {
        let fcp = gsm_select_response(fid);
        if fcp.len() >= 15 && (fid >> 8) != 0x3F && (fid >> 8) != 0x7F {
            fcp[14] as usize
        } else { 0 }
    }

    /// Tresc EF: najpierw mutowalny store (po UPDATE), potem statyczny default (ef_content).
    fn ef_bytes(&self, fid: u16) -> Vec<u8> {
        self.ef_data.get(&fid).cloned().unwrap_or_else(|| ef_content(fid))
    }

    /// SIM_STRICT: kod bledu dla komend case-2 wg swsim, albo None gdy OK.
    /// READ BINARY (B0): EF musi byc transparent (94 08), zakres w rozmiarze (67 00).
    /// READ RECORD (B2): EF musi byc rekordowy (94 08).
    fn case2_strict_sw(&self, ins: u8, p3: usize) -> Option<[u8; 2]> {
        let rl = self.rec_len(self.selected);
        match ins {
            0xB0 => {
                if rl != 0 { return Some([0x94, 0x08]); } // nie-transparent czytany jako binary
                let off = ((self.apdu[2] as usize) << 8) | self.apdu[3] as usize;
                let size = self.ef_bytes(self.selected).len();
                if off + p3 > size.max(0x20) { return Some([0x67, 0x00]); }
                None
            }
            0xB2 => {
                if rl == 0 { return Some([0x94, 0x08]); } // transparent czytany jako record
                None
            }
            _ => None,
        }
    }

    /// Dane dla komend case-2 (karta -> telefon), P3 bajtow.
    fn case2_data(&self, ins: u8, p3: usize) -> Vec<u8> {
        let src = match ins {
            0xC0 => self.gr.clone(),                    // GET RESPONSE: FCP z SELECT / RUN GSM ALGO
            0xF2 => gsm_select_response(self.selected), // STATUS: FCP biezacego DF
            0xB0 => {
                // READ BINARY: tresc wybranego EF od offsetu P1P2 (z mutowalnego store).
                let off = ((self.apdu[2] as usize) << 8) | self.apdu[3] as usize;
                if std::env::var("SIM_LOG").is_ok() {
                    eprintln!("[sim] READ BIN {:#06x} off={off} len={p3}", self.selected);
                }
                self.ef_bytes(self.selected).into_iter().skip(off).collect()
            }
            0xB2 => {
                // READ RECORD: P1 = nr rekordu (1-based, absolute gdy P2 bit2..0=4). Indeks po rec_len.
                let rl = self.rec_len(self.selected).max(1);
                let rec = (self.apdu[2] as usize).max(1);
                let off = (rec - 1) * rl;
                if std::env::var("SIM_LOG").is_ok() {
                    eprintln!("[sim] READ REC {:#06x} rec={rec} off={off} len={p3}", self.selected);
                }
                self.ef_bytes(self.selected).into_iter().skip(off).take(rl).collect()
            }
            0x12 => Vec::new(),                          // FETCH (Toolkit): brak proactive -> puste
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
