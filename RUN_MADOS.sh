#!/bin/bash
# Uruchamia emulator 3310 z MADos (bootujacy open-source OS DCT3).
# Sterowanie: strzalki gora/dol, Enter=menu/wybor, Backspace=anuluj, Esc=wyjscie.
export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
cd "$(dirname "$0")"
TIMER_FIQ_BIT=4 TIMER_IRQ=0 MBUSTIM_DIV=0 FW_FILE=.vendor/mados/STANDALONE.fls \
    cargo run --release -p nokia3310
