#!/bin/bash
# Emulator Nokia 3310 - OFICJALNY firmware (33DM101) + realny EEPROM (607) + model DSP.
# Sekwencja: self-test (DSP ready) -> animacja bootu -> logo Nokia hands ->
# custom animacja startowa (NOKIA SIEMENS PASJA GSM) -> idle (glowna petla zdarzen).
# Po animacji firmware czeka (jak telefon bez rejestracji GSM). Klawisz -> CONTACT SERVICE
# (brak pelnej gotowosci GSM). Pelne standby/menu wymaga emulacji transmisji GSM.
#
# Sterowanie: P=POWER, strzalki=gora/dol, Enter=wybor, Backspace=anuluj, Esc=wyjscie.
export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
cd "$(dirname "$0")"
FORCE_R5=1 DSP_FIQ_AT=50000 FORCE_REASON=2 FORCE_ST=1 cargo run --release -p nokia3310
