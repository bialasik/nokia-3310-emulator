//! Model DSP (TMS320 baseband) — wg MADos hw/dsp.c + dsp.h + dspblocks.c oraz
//! stockowego handlera IRQ_DSP (0x2bccac) i init (0x2bc880).
//!
//! Pamiec wspoldzielona _dsp[] @0x10000 (slowa 16-bit LE). Protokol bootu/uploadu:
//!  - MCU uploaduje naglowek bloku kodu do UPLOADHEADER (0x100F6, 5 slow), zeruje flage
//!    gotowosci 0x1106e8, po czym WLACZA DSP: IO_CTSI_DSP (0x20002) bit0 = 1.
//!  - DSP bootuje, wykonuje blok init (00), zada kolejnych blokow (01=MDI setup, 14=
//!    secondary) przez UPLOADREQUEST (0x100E2) + IRQ_DSP; MCU odpowiada UPLOADREPLY
//!    (0x100E4 = MORE 0x02 / FINISHED 0x04) i sygnalizuje DSP.
//!  - Gdy upload zakonczony, handler IRQ_DSP stocka ustawia 0x1106e8=1 **gdy
//!    UPLOADREPLY(0x100E4)==0**. To gotowosc DSP (warunek self-testu 14/15 -> nie-CONTACT).
//!
//! Nie wykonujemy kodu TMS320 — modelujemy STRONE DSP handshake'u: po WLACZENIU DSP
//! (rising-edge IO_CTSI_DSP bit0), po opoznieniu `fiq_at` krokow, DSP zglasza gotowosc:
//! UPLOADREPLY=0 (FINISHED) + IRQ_DSP (IRQL bit4). To wierne, ograniczone (bez L1/GSM).
//! Env DSP_FIQ_AT=N (0=wylaczony, np. MADos uruchamiamy bez DSP).
//!
//! Offsety shmem (od 0x10000, bajty): UPLOADREQUEST=0xE2, UPLOADREPLY=0xE4, status=0xDE,
//! UPLOADHEADER=0xF6, mailbox-typ=0xE0. Handshake bootstrap: 0xFE/0x100 (firmware czeka
//! `while(==0)` -> zwracamy 1), ID DSP 0x10002 (`while(==0xFFFF)` -> 0x0001).

pub const REG_CTSI_DSP: u32 = 0x0002_0002; // IO_CTSI_DSP: bit0 = DSP reset/enable
pub const DSP_UPLOADREPLY: u32 = 0x0001_00E4;
pub const REG_DSPIF: u32 = 0x0003_0000; // DSPIF: MCU pisze tu by KICKNAC DSP (przetworz mailbox)
pub const DSP_MDI_MAILBOX: u32 = 0x0001_00E0; // MCU->DSP mailbox flag (1=zajety, DSP czysci po odczycie)

pub struct Dsp {
    /// Po ilu krokach od wlaczenia DSP zglasza gotowosc (env DSP_FIQ_AT; 0=model off).
    fiq_at: u64,
    /// Ostatni stan bitu0 IO_CTSI_DSP (do detekcji zbocza narastajacego = wlaczenie).
    enabled: bool,
    /// Odliczanie do zgloszenia gotowosci (Some gdy DSP bootuje po wlaczeniu).
    countdown: Option<u64>,
    /// Tryb periodyczny: re-arm po kazdym wystrzale (env DSP_PERIODIC).
    periodic: bool,
    pub total_boots: u64,
    /// Odliczanie do konsumpcji mailboxa MDI po KICKU (zapis DSPIF 0x30000). Realny DSP
    /// czyta komende z mailboxa 0x100e0 i czysci go ->0; modelujemy to opoznione. None=brak.
    mdi_consume: Option<u64>,
    /// Liczba krokow opoznienia konsumpcji MDI (env DSP_MDI_DELAY, domyslnie 20).
    mdi_delay: u64,
    pub mdi_consumes: u64,
}

/// Akcja DSP do wykonania przez magistrale po ticku.
pub enum DspAction {
    /// Boot/upload gotowy: IRQ_DSP + UPLOADREPLY=0.
    Ready,
    /// MDI: DSP skonsumowal komende -> wyczysc mailbox 0x100e0 = 0.
    MailboxConsumed,
}

impl Dsp {
    pub fn new() -> Self {
        let fiq_at = std::env::var("DSP_FIQ_AT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        Self {
            fiq_at,
            enabled: false,
            countdown: None,
            periodic: std::env::var("DSP_PERIODIC").map(|v| v == "1").unwrap_or(false),
            total_boots: 0,
            mdi_consume: None,
            mdi_delay: std::env::var("DSP_MDI_DELAY").ok().and_then(|s| s.parse().ok()).unwrap_or(20),
            mdi_consumes: 0,
        }
    }

    /// Zapis do DSPIF (0x30000): MCU KICKUJE DSP by przetworzyl komende z mailboxa MDI.
    /// Realny DSP czyta mailbox 0x100e0, wykonuje, czysci go ->0. Modelujemy konsumpcje
    /// po `mdi_delay` krokach (env DSP_MDI_DELAY). Bez tego baseband widzi mailbox=1
    /// (zajety) wiecznie i nie wysyla kolejnych komend L1 -> utyka (boot nie do standby).
    pub fn on_dspif_write(&mut self, _val: u8) {
        if self.mdi_delay > 0 {
            self.mdi_consume = Some(self.mdi_delay);
        }
    }

    /// Zapis do IO_CTSI_DSP (0x20002): zbocze narastajace bit0 = wlaczenie DSP -> boot.
    pub fn on_ctsi_dsp_write(&mut self, val: u8) {
        let now = val & 0x01 != 0;
        if now && !self.enabled && self.fiq_at > 0 {
            // DSP wlaczony: rozpocznij boot, po fiq_at krokach zglosi gotowosc.
            self.countdown = Some(self.fiq_at);
            self.total_boots += 1;
        }
        self.enabled = now;
    }

    /// Tik (co krok CPU). Zwraca Some(DspReady) gdy DSP zglasza gotowosc/zdarzenie.
    /// W trybie periodycznym (env DSP_PERIODIC) re-arm po wystrzale: DSP wysyla
    /// przerwania cyklicznie (jak realny - frame sync / MDI), nie raz. To moze zaspokoic
    /// self-test czekajacy na zdarzenie DSP (v6.39 bit2 0x11ff15).
    pub fn tick(&mut self) -> Option<DspAction> {
        if let Some(c) = self.countdown {
            if c <= 1 {
                self.countdown = if self.periodic { Some(self.fiq_at) } else { None };
                return Some(DspAction::Ready);
            }
            self.countdown = Some(c - 1);
        }
        // Konsumpcja mailboxa MDI po kicku DSPIF (priorytet po boot-ready).
        if let Some(c) = self.mdi_consume {
            if c <= 1 {
                self.mdi_consume = None;
                self.mdi_consumes += 1;
                return Some(DspAction::MailboxConsumed);
            }
            self.mdi_consume = Some(c - 1);
        }
        None
    }

    /// Korekta odczytu shmem DSP (handshake bootstrap + ID). Wartosc `v` to surowa
    /// zawartosc mmio[]; zwracamy skorygowana gdy firmware czeka na gotowosc sprzetu.
    pub fn read_fixup(&self, addr: u32, v: u32) -> u32 {
        match addr {
            // bootstrap handshake: firmware czeka `while(*==0)` -> 1
            0x0001_00FE | 0x0001_0100 if v == 0 => 1,
            // ID/wersja DSP: firmware czeka `while(*==0xFFFF)` -> 0x0001
            0x0001_0002 if v == 0xFFFF => 0x0001,
            _ => v,
        }
    }
}

impl Default for Dsp {
    fn default() -> Self {
        Self::new()
    }
}
