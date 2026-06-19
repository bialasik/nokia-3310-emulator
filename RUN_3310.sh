#!/bin/bash
# Nokia 3310 - boot OFICJALNEGO firmware 6.39 DO EKRANU "WPISZ PIN".
#
# ZWERYFIKOWANY PRZEZ UŻYTKOWNIKA (2026-06): ta dokładna kombinacja env daje
# pełny boot: self-test OK -> animacja -> ekran wpisywania PIN (działa wpisywanie
# cyfr + akceptacja). NIE zmieniać tych zmiennych - każda jest potrzebna:
#   DSP_FIQ_AT=20000   - wierny model DSP zgłasza gotowość (self-test 14/15 OK, zdejmuje reboot/CONTACT)
#   TIMER_AUTORELOAD=1 - v6.39 wymaga periodycznego TMR0 (stały target) - bez tego timery OS nie tykają
#   SELFTEST_SUB=1     - subskrypcja wyników self-testu (pub/sub) -> self-test realnie completuje
#   REG_ALL=1          - bitmapa dostarczalności indykacji (wszystkie taski) -> eventy MMI płyną
#   ST_PASS=1          - bramka watchdog/CONTACT (0x11ff1f pass) -> telefon nie wyłącza się
#   SIM_ATR=1          - włącza model ATR/APDU karty SIM -> init SIM -> prompt PIN
#   SIM_IMSI=001011234567890 - test-SIM MCC=001 omija SIMLOCK (po PIN: kod 12345
#                      akceptowany); IMSI użyty przy testach akceptacji PIN
#
# Firmware: crates/rom/ MUSI mieć TYLKO 6.39 ("3310 v6.39 Converted MCU+PPM B.fls",
# loader pokazuje `firmware id : 39 23-12-04 NHM-5`) + EEPROM 607. NIE trzymać 33DM101
# (6.33) w crates/rom/ - dwa pliki tej samej wielkości => loader wybiera niedeterministycznie.
#
# Sterowanie: cyfry 0-9 (klawiatura/numpad), Enter=OK/wybór, Backspace=C/kasuj, P=POWER
# (POWER auto-trzymany ~2s na starcie, nie trzeba dotykać), Esc=wyjście.
export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
cd "$(dirname "$0")"
DSP_FIQ_AT=20000 TIMER_AUTORELOAD=1 SELFTEST_SUB=1 REG_ALL=1 ST_PASS=1 SIM_ATR=1 \
  SIM_IMSI=001011234567890 \
  cargo run --release -p nokia3310
