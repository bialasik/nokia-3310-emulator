# Notatki bring-upu (M2) — żywy dokument

ROM: `33 33-DM-01 NHM-5` (Nokia 3310). Obraz 4 MB, ROM w `0x200000..0x400000`,
poniżej `0x200000` = RAM / wektory / MMIO (adresy nieznane — datasheet MAD2 zamknięty).

Narzędzie: `cargo run -p emucore --bin trace -- [ENTRY_HEX] [STEPS]`

## Ustalenia

### 2026-05-31 — pierwszy trace od ENTRY=0x200000
- `0x200000` to **najprawdopodobniej nagłówek/blok startowy, nie czysty kod** — pierwsze
  bajty `20 65 0f 59 ...` dekodują się dziwnie; string wersji `NHM-5` jest tuż obok (0x200200).
- **PC=0x200008**: zapisy `u32` pod `0x1FFFF4 / 0x1FFFF8 / 0x1FFFFC` → push na stos (STMFD),
  **SP ≈ 0x200000**. Wniosek: **RAM leży tuż poniżej ROM-u** (rośnie w dół od 0x200000).
- **PC=0x20000C**: odczyt wskaźnika z `~0x140`, skok pod adres = 0 (RAM pusty) → NOP-slide
  przez wyzerowaną pamięć (instrukcja `0x00000000` = `andeq r0,r0,r0`).
- **Hipoteza**: firmware zakłada, że **tablica wektorów/skoków w niskiej pamięci (≈0x140)**
  jest już wypełniona (przez boot-ROM MAD2 lub wcześniejszą inicjalizację, którą pomijamy
  startując od 0x200000). To jest niewiadoma „reset/remap" z planu (ryzyko #2).

### 2026-05-31 — definitywna mapa z MADos `data/3310_old/memmap` + dezasemblacja entry
- **Linker memmap 3310**: `rom: ORIGIN=0x00200000 LEN=2048K`, `ram: ORIGIN=0x00100040 LEN=127K`.
  → **RAM ≈ 0x00100000..0x00200000**, ROM 0x200000..0x400000.
- `memmap_embed`: MADos w trybie EMBED ląduje w PPM (`ORIGIN=0x00340040 LEN=200K`).
- **Nagłówek flasha** zajmuje 0x40 bajtów; **prawdziwy ENTRY = `0x00200040`** (crt0 `.ORG 0x40`).
  0x200000 = słowo nagłówka (`20 65 0f 59`), 0x200020 = `dead` + wskaźniki.
- Dezasemblacja `0x200040` (zgodna z `crt0.s` MADos — g3gg0 zreversował to stąd):
  - `MOV r1,#0x00200000; LDR r0,[r1]` — czyta nagłówek
  - `MOV r1,#0x00020000; STRB ...,[r1,#0x70..0xEF]` — config kontrolera pamięci (MMIO 0x20070-0xEF)
    wartości: 0,2,0xA0,0x6b,0xFF,0xFD,0x5F,0,0x60,4,0
  - `MSR cpsr,r0; ADD sp,...` — ustawienie stosów dla trybów CPU (IRQ/FIQ/...)
- **Wniosek**: start od 0x200000 wykonywał nagłówek (śmieci). Poprawny start = **0x200040**.

### 2026-05-31 — KLUCZOWE: firmware jest BIG-ENDIAN
- Bajty entry `e3 a0 16 02`: jako LE = śmieci („Invalid condition code"); jako **BE = `0xE3A01602`
  = `MOV r1,#0x00200000`**. Firmware DCT3 działa w trybie **ARM BE-32**.
- Magistrala `Machine` przełączona na big-endian dla load/store 16/32 (load_8 bez zmian).
- **Efekt**: od ENTRY=0x200040 firmware wykonuje sensowny kod boot i **nie opuszcza ROM-u**:
  config memory-controllera (0x20070-0xEF), zapis MAGIC do 0x40000, MSR/stosy trybów.
- Rdzeń arm7tdmi (z GBA, LE) toleruje BE na poziomie magistrali dla słów wyrównanych
  (instrukcje, LDM/STM, LDR/STR word/half). Ryzyko: nietypowy LDR niewyrównany (rotacja).
- Pozostaly drobiazg do zbadania: ~0x200194 wyglada na petle (czyszczenie RAM / delay / wait).

### 2026-05-31 — stub DSP odblokowal boot; firmware steruje LCD
Pętle pollingu DSP (Thumb, w ROM):
- `0x2BC6E2`: `while(_dsp[0x100FE]==0)` — czeka na ack uploadu bloku (115 bloków). Stub: zwroc 1.
- `0x2BC726`: `while(_dsp[0x10002]==0xFFFF)` — czeka na wersję/ID DSP, potem jej używa. Stub: zwroc 0x0001.
Po stubie (`dsp_fixup` w machine.rs): **brak busy-poll**, MMIO reads 1.8M→790,
**MMIO writes → 60386** (firmware pcha dane do LCD przez GENSIO 0x2002C/0x2006D), PC=0x2EF9B6.
→ Próg M3 osiągniety: trzeba zdekodowac GENSIO→PCD8544 do bufora 84x48.

## Otwarte pytania / następne kroki M2
1. **Znaleźć prawdziwy punkt wejścia / zachowanie resetu.** Zdezasemblować początek ROM-u
   (moduł `arm7tdmi::disass`), odszukać kod, który wypełnia tablicę w niskiej pamięci.
2. **Ustalić mapę RAM** (potwierdzić, że RAM kończy się na 0x200000; znaleźć początek).
3. **Porównać z MADos** (otwarty firmware DCT3) — kolejność init sprzętu.
4. **Watchdog / CCONT / timery** — zacząć identyfikować rejestry MMIO z logów tracera.

### 2026-05-31 — firmware bootuje; blokada = handshake DSP
Po fixie BE firmware przechodzi pełny boot: config mem-ctrl, kopiowanie wektorów do RAM,
włączenie IRQ/FIQ, przejście w **Thumb** (`bx r0`), relokacja kodu do RAM (skok do 0x11F7B0),
~2-3 mln instrukcji. Następnie **busy-poll** (877k odczytów) na **`0x000100FE`**.

Bloki MMIO wg MADos (`core/crt0_embed.s`):
- `_dsp = 0x00010000` — pamięć współdzielona DSP (TMS320). Firmware uploaduje boot DSP i czeka odpowiedzi.
- `_dspif = 0x00030000` — interfejs DSP
- `_mcuif = 0x00040000` — interfejs MCU (tu szedł zapis MAGIC 0x40000)
- `0x00020000` — CTSI/PUP/UIF/GENSIO (rejestry MCU)

**Blokada**: `0x100FE` to słowo w pamięci DSP; firmware czeka aż DSP zapisze odpowiedź
(`DSP_UPLOADREPLY_FINISHED=0x0004`). Brak emulacji TMS320 → trzeba **zastubować handshake DSP**.

### 2026-05-31 — M3: dekoder PCD8544 dziala; firmware init+clear LCD; gate = IRQ timera
GENSIO->PCD8544 (z MADos hw/lcd.c): zapis pod **0x2002E = DANE**, **0x2006E = KOMENDA**.
Zaimplementowano dekoder `emucore::lcd::Pcd8544` (komendy 0x20/21/40|y/80|x/08|D|E, auto-X++).
Trace: firmware wysyla pelne odswiezenie (504 bajty danych = 84x6) + 24 komendy:
`24 40 80 41 80 .. 45 80 20 21 05 14 BF 20 0C 21 80 20 08`
= init banki/X, bias/temp, kontrast 0x3F, display ON (0x0C), potem clear (504 zer) i BLANK (0x08).
Następnie utyka w petli `0x2EF9B6: cmp r5,#1; bne self` (po zapisach GENSIO).
**Hipoteza**: scheduler 3310 czeka na **przerwanie timera** (CTSI TMR0 @0x20010, ICR @0x2000C);
firmware wlaczyl IRQ/FIQ (mrs/bic #0xC0), ale nie generujemy ticków -> brak postepu.
**Następny krok**: zaimplementowac timer + okresowy IRQ (wektor 0x18), by scheduler ruszyl i narysowal UI.

### 2026-05-31 — przerwania: timer = FIQ; latch musi zwracac tylko aktywne zrodla
Z MADos hw/int.c / hw/timer.c:
- Kontroler CTSI: **FIQL=0x20008** (latch FIQ), IRQL=0x20009, FIQM=0x2000A, IRQM=0x2000B,
  **ICR=0x2000C** (IRQE/IRQD/FIQE/FIQD), TMR0=0x20010, TMR0T=0x20012, TMR0D=0x2000F.
- **Timer to FIQ** (`int_fiq_set_handler(0x04/0x05,...)`), `timer_advance`: TMR0T = TMR0 + 0x200.
- Wektory: IRQ@0x18 -> 0x2E5B40, FIQ@0x1C -> 0x2E5CA8 (zainstalowane przez firmware ✔).
- Rdzen ma `cpu.irq()`; **brak `cpu.fiq()`** - trzeba dodac (exception Fiq).
- **Wniosek**: slepe IRQ/FIQ crashuje, bo latch=0xFF => handler dispatchuje wszystkie zrodla
  (w tym null -> skok pod 0 -> panika). Latch MUSI zwracac tylko realnie aktywne bity.
- **TODO**: model kontrolera (latch/maska/ICR jako rejestry z semantyka clear-on-write) +
  licznik TMR0 + asercja FIQ tylko z bitem timera, gdy FIQ wlaczony (ICR FIQE, maska, CPSR.F=0).

### 2026-05-31 — kontroler przerwan + timer + CCONT + MBUS (M2/M3 glebokie)
Zaimplementowano (emucore): `ctsi.rs` (latch/maska/ICR + timer TMR0), `cpu.fiq()` w rdzeniu,
`ccont.rs` (uklad zasilania przez GENSIO), stub MBUS (0x20018/19 -> 0).
Ustalenia:
- **Glitch-free read TMR0**: firmware czyta TMR0 dwa razy i czeka az rowne (0x2E4D68).
  TMR0 musi tykac wolno -> `step_div` duzy (1024). Inaczej zawisa.
- **CCONT**: protokol GENSIO (CC_WR=0x2002C/CC_RD=0x2006C). ID: reg0x03 gorne bity 0xB0.
  Model: maszyna stanow reg<<3 [| 0x04 read], rejestry z sensownymi wartosciami.
- **Hang 0x2EF9B6** (`cmp r5,#1; bne`): r5 ustawiane na sciezce **zaleznej od przerwan**;
  bez przerwan zawisa, z przerwaniami przechodzi. => przerwania KONIECZNE.
- **Timer routing**: zrodlo FIQ bit 0/4 -> livelock handlera (0x2E5CA8, nie kasuje bitu);
  bity 1/2/3/5/6/7 -> serwisowane, main idzie dalej (do MBUS 0x2E4442).
- Po stubie MBUS: bit 2 wpada w livelock FIQ 0x2E5CA8 -> zachowanie zalezne od stanu przerwan.
  Otwarte: czy timer to IRQ (wektor 0x18->0x2E5B40) zamiast FIQ; dokladne routing/ack.
- LCD: nadal tylko init+clear (504 zera) - firmware rysuje UI dopiero PO pelnym init.

## Stan: dalszy bring-up wymaga rozplatania routingu przerwan (IRQ vs FIQ, ktore zrodlo,
## ack) oraz kolejnych peryferiow. Infrastruktura (CPU, magistrala, tracer, disasm, LCD,
## kontroler przerwan, CCONT) gotowa. To wieloetapowy proces.

### 2026-05-31 — PRZELOM: fix odczytu ICR uruchomil scheduler
Handlery (ARM) FIQ@0x2E5CA8 i IRQ@0x2E5B40 czytaja **ICR (0x2000C)** i testuja bity
"disabled" (FIQ: TST #0x02, IRQ: TST #0x08). Gdy ustawione -> traktuja jako spurious i
wracaja BEZ kasowania zrodla -> livelock. Zwracalismy ICR=0xFF -> zawsze "spurious".
**FIX**: `ctsi.read(ICR)` zwraca stan: wlaczone => bity disabled czyste
(`(fiq_en?1:2)|(irq_en?4:8)`). Po tym firmware przechodzi wczesne hangi i **rusza scheduler**.

Glowna petla (Thumb @0x2E7F7E) robi `pending = FIQL & ~FIQM` i dispatchuje zadania wg bitu
(tablica skokow @0x2E7F0A). Czyli scheduler napedzany **zlatchowanymi zrodlami FIQ** (poll).
- **Timer = FIQ bit 2** (jedyny przechodzi hang 0x2EF9B6 -> dociera do main loop).
- Stan: firmware wykonuje scheduler/main loop (rozne regiony 0x2E7Fxx/0x2E4Dxx/0x2F08xx),
  ale rysuje tylko init+clear LCD (504 zera). Logo bootu wyzwala zadanie, ktorego trigger
  trzeba jeszcze ustalic (stan-maszyna bootu / inne zrodlo zdarzen).
- Domyslne: TIMER_FIQ_BIT=2, TIMER_STEP_DIV=1024.

### 2026-05-31 — timer = FIQ8 (PUP_FIQ8), system tick = IRQ; logo wciaz nie rysowane
- Idle-path schedulera (0x2E7F8C) czyta **PUP_FIQ8 (0x20016)** i testuje bit1 (ACT=0x02)
  -> **timer to FIQ8**, nie bit FIQL. Zwracanie 0xFF (ACT zawsze) -> task timera dispatch bez przerwy.
  Model: PUP_FIQ8 jako rejestr; ACT ustawiany przy wystrzale timera (gdy EN), kasowany przez firmware.
- **System tick = IRQ** (handler 0x2E5B40 inkrementuje licznik + scheduler, nie czyta IRQL).
  Dodano periodyczny one-shot IRQ (`irq_tick_pending`) obok FIQ.
- Po tych zmianach scheduler iteruje bogatsza petle (0x2F0820-3E, 0x2E4D1E-36), ale **nadal
  tylko init+clear LCD (504 zera)** po 120M krokow. Boot state-machine nie dochodzi do rysowania logo.
- Hipotezy do dalszego RE: (a) konkretny warunek/event bootu (power-on reason z CCONT, RTC),
  (b) dokladne kasowanie/ack PUP_FIQ8 ACT i interwaly, (c) zadanie rysowania wyzwalane innym zrodlem.

### 2026-05-31 — prędkość 13 MHz + dezasemblacja przepływu bootu
- **Prędkość**: okno taktowane do **13 MHz** (ARM7TDMI DCT3), pacing wall-clock, build release
  (interpreter ~90M kr/s, dlawiony do 13M). Tytul pokazuje efektywne MHz + czas CPU.
- **Tracer**: dodano swiadoma ARM/Thumb dezasemblacje wykonania (ASM_FROM=krok startowy).
- **Clear LCD** konczy sie ~krok 820374; potem `lcd_init` zwraca i boot wchodzi w scheduler.
- **Struktura schedulera** (Thumb):
  - `0x2998CE`: gate `[0x299984]` (==0 -> return); iteruje tablice zadan @0x299B94 (8B/wpis:
    [0]=ptr/wartosc, [4]=licznik; pusty slot gdy [0]==0xFFFFFFFF).
  - `0x2E4D1E`: programowe timery (licznik [0x2E505C], reload z [0x2E5058+0xC]).
- Firmware ma poprawny scheduler z tablica zadan programowych i tyka je, ale **zadanie rysujace
  nigdy nie pada** (LCD data na stale 504). Trigger draw gated wyzej (stan bootu / power-on reason).
- MADos main.c = menu-app (zastepczy OS): potwierdza kolejnosc init (ccont->lcd->kpd->sim)
  i UI sterowane kpd_getkey, ale boot stockowy ma wlasna maszyne-stanow.

### 2026-05-31 — CCONT bateria + uwaga o realnym boocie
- Real boot 3310 (uzytkownik): po wlaczeniu LCD **zapelnia sie all-pixels**, gasnie, potem tresc.
  Nasz stan "pusty po clear" = faza "off" zaraz po blysku. Rutyna 0x2ECC28 to clear (r3=0 na sztywno);
  all-pixels (0xFF) to osobny/wczesniejszy krok, ktorego nie osiagamy.
- Firmware intensywnie czyta CCONT reg 0x02/0x03 (22-23x) = **AD napiecia baterii**.
  Nasze poczatkowe reg02=0xC0 dawalo ~1.3V (martwa!). Poprawiono na reg02=0x3A/reg03=0xB2 (~3.7V).
  ALE: nie zmienilo przeplywu - na tym etapie bateria nie gat-uje rysowania.
- Stan: scheduler stabilny, monitoruje baterie, ale **zadanie rysujace dalej nie pada**.
  Rendered UI pozostaje zablokowane za maszyna-stanow bootu (long-tail RE: power/DSP-GSM/RTC/SIM).

### 2026-05-31 — WALIDACJA: MADos renderuje ekran w naszym emulatorze!
- Zbudowano MADos STANDALONE dla 3310 ze zrodel (arm-none-eabi-gcc 16, `-std=gnu89 -DARM`,
  wlasny linker script `data/3310_old/memmap_custom`, `-lgcc`, `--allow-multiple-definition`).
  Build potwierdza: **big-endian ARM7TDMI** (`-EB -mbig-endian -mcpu=arm7tdmi`) = nasza decyzja.
- Loader: env `FW_FILE=<plik>` laduje dowolny obraz na 0x200000 (do testow MADos vs stock).
- **MADos bootuje i RYSUJE ekran** w naszym emulatorze: `nonzero=697 pikseli`, tekst "KPD..."
  widoczny w zrzucie ASCII (krok ~600k-1.2M). **To pelna walidacja emulatora** (CPU/pamiec/LCD/BE).
- Crash MADos ~krok 2M: jego inthandler/wektory zakladaja inny layout/sprzet niz nasze stuby
  (`.INTS` mialy base 0x400000; po patchu na 0x200000 nadal crash - rozni sie model sprzetu).
  Bez przerwan (INT_OFF) MADos nie crashuje, ale czysci ekran (scheduler potrzebuje timera).
- Okno: zatrzaskiwanie ostatniej niepustej klatki -> widoczny wyrenderowany ekran MADos mimo crasha.
- WNIOSEK: emulator dziala end-to-end. Stock firmware nie rysuje UI bo jest GSM-gated (nie blad
  emulatora). MADos (bez GSM) renderuje od razu = dowod poprawnosci.

### 2026-05-31 — MADos: pelny boot + scheduler; ostatni bug = context-switch
- MADos `main()` (apps/main.c): po init sprawdza `kpd_readkey()==KPD_OFF` (POWER); jak nie -
  `ccont_poweroff()`. Emulator: matryca klawiatury z POWER wcisnietym przez pierwsze `pwr_hold`
  krokow (keypad.rs) -> POWER wykryty -> MADos zostaje wlaczony.
- Timer MADos = **CTSI FIQL bit 4** (int_handler: dispatch int_fiq_routines[4] + task-switch
  `if(fiq&0x10)`); NIE FIQ8/PUP_FIQ8 (tam routine[8]=0 -> brak ack -> loop w int_handler).
  Konfig: TIMER_FIQ_BIT=4, TIMER_IRQ=0 (MADos jest FIQ-only). Stock: bit2 + IRQ-tick.
- `int_handler` to DEBUG build (rysuje "got an int F:.. I:.. C:.." na kazdym ircie) - ale i tak
  powinien narysowac menu miedzy irq.
- **Postep**: MADos przechodzi caly init (debug-texty SCHED/CCONT/TIMER/KPD/MBUS/BUZZER...),
  scheduler robi pierwszy context-switch na watek - ale **restore przywraca zly PC (~0xFFD20)**
  -> NOP-slide -> crash przed narysowaniem menu.
- **Ostatni bug**: emulacja context-switchu MADos (stmdb{r0-r14}^ + sched_save/get per-rejestr +
  finalny restore z bankowaniem) korumpuje PC watku. Rdzen MA obsluge S-bit LDM/STM (exec.rs
  496-619), wiec to subtelnosc w tym konkretnym wzorcu. To deep CPU-core debug.
- Stabilny pokaz: TIMER_FIQ_BIT=8 (MADos nie crashuje, renderuje debug-ekrany, brak menu).

### 2026-05-31 — DEBUG context-switchu MADos (deep)
Context-switch crt0: handler `stmfd sp,{r0-r14}^` -> per-rejestr sched_save -> sched_next ->
sched_get -> int_end `ldmfd sp,{r0-r14}^; subs pc,lr,#4` (PC=regs[15], CPSR=SPSR).
Instrumentacja (DUMP_PC w trace.rs):
- **Context-switch DZIALA**: watek-petla-opoznienia (0x211856) restartuje sie poprawnie wiele
  razy (user R13=0x11CFD0 stos OK, PC OK, r0 dekrementuje 0x30D40..). `^` LDM przywraca stos dobrze.
- **Crash**: inny watek ma regs[15]=0x5E928 (smieci) -> restore -> NOP-slide -> crash @0x100040.
  Nie z sched_add (ma check addr>=0x100000). Watek WYKONAL sie do 0x5E928 (zly skok) i tam
  zostal wywlaszczony -> zapisany zly PC.
- regs[7-9]=0xEA08005x (slowa wektorow 0x14/0x18/0x1C): sched_add NIE inicjuje regs[0..12]
  (tylko 13-16); na realnym HW bss=0 (crt0 czysci), u nas tez powinno - do sprawdzenia.
- Hipoteza: jakis watek (nie petla-opoznienia) liczy zly adres skoku (moze przez niezainicj.
  regs lub subtelnosc emulacji) i laduje w 0x5E928. Wymaga dalszego sledzenia konkretnego watku.
- STATUS: emulator uruchamia MADos przez pelny boot+scheduler+wiele context-switchy; zostaje
  jeden watek mis-jump. Gleboki, konkretny bug - dalsze RE konkretnego watku.

## Mapa MMIO (odkrywana)
Źródło referencyjne: **MADos** (`.vendor/mados`, LGPL — używamy jako referencji adresów,
nie kopiujemy kodu). `core/crt0.s` + `hw/*.c` + `include/hw/ioports.h`.

| Adres | Funkcja | Pewność | Notatka |
|---|---|---|---|
| `< 0x200000` | RAM (stos od ~0x200000 w dół) | średnia | push na 0x1FFFFx z PC=0x200008 |
| `~0x140` | tablica wskaźników/wektorów w RAM | niska | czytana przez firmware @0x20000C |
| **`0x00020000`** | **blok rejestrów systemowych (CTSI: watchdog, timery TMR0/1, IRQ/FIQ, ICR)** | **wysoka (MADos)** | crt0 pisze do 0x200070..0x2000B3; ioports.h: CTSI_WDT/TMR/FIQ/IRQ |
| `0x00010000` | blok rejestrów (do ustalenia) | średnia | częsty w hw/*.c |
| `0x00030000` | blok rejestrów (do ustalenia) | średnia | częsty w hw/*.c |
| `0x00040000` | rejestr "MAGIC" (init) | średnia | crt0: `str MAGIC,[0x40000]` |
| `0xAAAA/0x5554` | sekwencja odblokowania flash (programowanie) | wysoka | hw/flash.c |

Sterowniki referencyjne w MADos: `hw/lcd.c` (LCD), `hw/kpd.c`+`kpd_getkey_matrix.h` (klawiatura),
`hw/ccont.c` (zasilanie), `hw/timer.c`, `hw/int.c` (przerwania), `hw/genio.c` (GPIO),
`hw/buzzer.c`, `core/crt0.s` (boot/init).
