#!/usr/bin/env python3
"""Brute-force config SIMLOCK. Patch .data 0x2F33B8 -> RAM 0x10EEB0 (18B), boot+PIN, OBIEKTYWNY werdykt.
Raportuje: boot (czy doszedl do PIN 0x2E1D04), werdykt (ACCEPT 0x2726BE / REJECT 0x2B0540 / BRAK),
kod powodu byte[0x10FAC7] (0xA3=nicht angenommen, 0x03=einsetzen, 0x00=brak)."""
import subprocess, os, sys

BIN = os.path.join(os.path.dirname(os.path.abspath(__file__)), "target/release/dbg")
ADDR = 0x2F33B8
ENV = dict(os.environ, DSP_FIQ_AT="20000", TIMER_AUTORELOAD="1", SELFTEST_SUB="1",
           REG_ALL="1", ST_PASS="1", SIM_ATR="1")
ORIG = [0xEE]*8 + [0xFF]*10

def test(cfg):
    cmds = "".join(f"wb {hex(ADDR+i)} {hex(b)}\n" for i, b in enumerate(cfg))
    cmds += "until 0x2E1D04 25000000\nrun 11000000\npin 1234\nrb 0x10FAC7 1\nq\n"
    try:
        r = subprocess.run([BIN], input=cmds, capture_output=True, text=True, timeout=110, env=ENV)
    except subprocess.TimeoutExpired:
        return ("TIMEOUT", "", "")
    out = r.stdout + r.stderr
    boot = "no"
    verdict = "?"
    reason = "?"
    for line in out.splitlines():
        if "until 0x2E1D04" in line:
            boot = "PIN" if "TRAFIONO" in line else "no"
        if "WERDYKT" in line:
            if "ACCEPT" in line: verdict = "ACCEPT***"
            elif "REJECT" in line: verdict = "REJECT"
            else: verdict = "BRAK"
        if "0x10FAC7" in line:
            reason = line.split("0x10FAC7:")[-1].strip()
    return (boot, verdict, reason)

if __name__ == "__main__":
    print(f"BIN={BIN}\nBASELINE orig:", test(ORIG), flush=True)
    # Sweep pojedynczego bajtu: dla kazdej pozycji 0-17, kilka wartosci.
    VALS = [0x00, 0x11, 0x94, 0x95, 0x01]
    for i in range(18):
        for v in VALS:
            if v == ORIG[i]: continue
            cfg = ORIG[:]; cfg[i] = v
            boot, verd, reason = test(cfg)
            mark = "  <<<<<" if "ACCEPT" in verd else ("  (boot+verdykt zmiana)" if boot=="PIN" and verd!="REJECT" else "")
            print(f"byte[{i:2d}]={v:#04x}  boot={boot:3s} werdykt={verd:9s} powod={reason}{mark}", flush=True)
