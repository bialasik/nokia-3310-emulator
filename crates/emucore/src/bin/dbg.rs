//! Interaktywny debugger REPL podlaczony do emulatora 3310. Czyta komendy ze stdin
//! (linia po linii) i wykonuje na zywo - bez rebuildu mozna testowac rozne mozliwosci:
//! czytac/pisac pamiec+rejestry, force-read ("co jesli byte[X]=V"), breakpointy, klawisze,
//! zrzut ekranu. Uzycie (skryptowo): `echo "komendy..." | dbg` lub interaktywnie.
//!
//! Env jak RUN_3310.sh (DSP_FIQ_AT=20000 TIMER_AUTORELOAD=1 SELFTEST_SUB=1 REG_ALL=1 ST_PASS=1 SIM_ATR=1).
//!
//! Komendy:
//!   run N            - wykonaj N krokow
//!   until ADDR [MAX] - wykonaj az PC==ADDR (domyslnie MAX=50M)
//!   untilany A,B,..  - az PC trafi ktorykolwiek
//!   pc | state       - PC / stan (pc,crashed,lcd,clock,kroki)
//!   reg              - rejestry r0..r15
//!   setreg N VAL
//!   rb ADDR [CNT]    - czytaj CNT bajtow (hex). rh ADDR=halfword. rw ADDR=word(32)
//!   wb ADDR VAL      - zapisz bajt. ww ADDR VAL=word
//!   force ADDR VAL   - wymus odczyt ADDR=VAL (live). unforce ADDR. clearf. forces
//!   key NAME         - tap (press+release+run). NAME: 0-9,*,#,up,down,select,back,power
//!   keydown/keyup N
//!   pin 1234         - wpisz cyfry + select (OK)
//!   screen | lcd     - zrzut ekranu ASCII
//!   echo TEKST       - wypisz (znacznik)
//!   q | quit

use emucore::{EmuKey, Emulator};

fn parse_num(s: &str) -> Option<u32> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(h, 16).ok()
    } else {
        s.parse::<u32>().ok().or_else(|| u32::from_str_radix(s, 16).ok())
    }
}
fn parse_u64(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x") { u64::from_str_radix(h, 16).ok() } else { s.parse().ok() }
}

fn keycode(name: &str) -> Option<EmuKey> {
    Some(match name.trim() {
        "up" => EmuKey::Up, "down" => EmuKey::Down, "select" | "ok" | "menu" => EmuKey::Select,
        "back" | "c" => EmuKey::Back, "power" => EmuKey::Power, "*" | "star" => EmuKey::Star,
        "#" | "hash" => EmuKey::Hash,
        d if d.len() == 1 && d.chars().next().unwrap().is_ascii_digit() =>
            EmuKey::Digit(d.parse::<u8>().ok()?),
        _ => return None,
    })
}

fn dump_screen(emu: &Emulator) {
    println!("--- LCD (pc={:#08X}) ---", emu.pc());
    for y in 0..emucore::emu::LCD_H {
        let mut line = String::with_capacity(emucore::emu::LCD_W + 2);
        line.push('|');
        for x in 0..emucore::emu::LCD_W {
            line.push(if emu.lcd_get(x, y) { '#' } else { ' ' });
        }
        line.push('|');
        println!("{line}");
    }
}

fn main() {
    let mut emu = match Emulator::new() {
        Ok(e) => e,
        Err(e) => { eprintln!("blad: {e}"); std::process::exit(1); }
    };
    eprintln!("[dbg] firmware: {} | gotowy. Komendy ze stdin.", emu.firmware_id);

    use std::io::BufRead;
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = match line { Ok(l) => l, Err(_) => break };
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') { continue; }
        let mut it = line.split_whitespace();
        let cmd = it.next().unwrap_or("");
        let args: Vec<&str> = it.collect();
        match cmd {
            "q" | "quit" | "exit" => break,
            "echo" => println!(">> {}", args.join(" ")),
            "run" | "r" => {
                let n = args.first().and_then(|s| parse_u64(s)).unwrap_or(1_000_000);
                emu.run_steps(n);
                println!("[run {n}] pc={:#08X} crashed={} kroki={}", emu.pc(), emu.crashed, emu.total_steps());
            }
            "until" => {
                if let Some(a) = args.first().and_then(|s| parse_num(s)) {
                    let max = args.get(1).and_then(|s| parse_u64(s)).unwrap_or(50_000_000);
                    let (hit, done) = emu.run_until(a, max);
                    println!("[until {a:#08X}] {} po {done} krokach, pc={:#08X}",
                        if hit { "TRAFIONO" } else { "MAX/brak" }, emu.pc());
                } else { println!("err: until ADDR"); }
            }
            "untilany" => {
                let ts: Vec<u32> = args.join("").split(',').filter_map(parse_num).collect();
                let max = 50_000_000u64;
                let (hit, done) = emu.run_until_any(&ts, max);
                println!("[untilany] {:?} po {done}, pc={:#08X}", hit.map(|p| format!("{p:#08X}")), emu.pc());
            }
            "pc" => println!("pc={:#08X}", emu.pc()),
            "cyc" => {
                let (s, c) = (emu.total_steps(), emu.total_cycles());
                println!("steps={s} cycles={c} CPI={:.3}", c as f64 / s.max(1) as f64);
            }
            "state" => println!("pc={:#08X} crashed={} lcd_writes={} clock={:?} kroki={}",
                emu.pc(), emu.crashed, emu.lcd_data_writes(), emu.clock(), emu.total_steps()),
            "reg" => {
                for r in 0..16 {
                    print!("r{r:<2}={:08X} ", emu.get_reg(r));
                    if r % 4 == 3 { println!(); }
                }
            }
            "setreg" => {
                if let (Some(n), Some(v)) = (args.first().and_then(|s| parse_num(s)), args.get(1).and_then(|s| parse_num(s))) {
                    emu.set_reg(n as usize, v); println!("r{n}={v:#X}");
                }
            }
            "rb" => {
                if let Some(a) = args.first().and_then(|s| parse_num(s)) {
                    let cnt = args.get(1).and_then(|s| parse_num(s)).unwrap_or(16);
                    print!("{a:#08X}:");
                    for i in 0..cnt { print!(" {:02X}", emu.read8(a + i)); }
                    println!();
                }
            }
            "rh" => { if let Some(a) = args.first().and_then(|s| parse_num(s)) { println!("{a:#08X}={:#06X}", emu.read16(a)); } }
            "rw" => { if let Some(a) = args.first().and_then(|s| parse_num(s)) { println!("{a:#08X}={:#010X}", emu.read32(a)); } }
            "wb" => {
                if let (Some(a), Some(v)) = (args.first().and_then(|s| parse_num(s)), args.get(1).and_then(|s| parse_num(s))) {
                    emu.write8(a, v as u8); println!("[{a:#08X}]={:#04X}", v as u8);
                }
            }
            "ww" => {
                if let (Some(a), Some(v)) = (args.first().and_then(|s| parse_num(s)), args.get(1).and_then(|s| parse_num(s))) {
                    for i in 0..4 { emu.write8(a + i, (v >> (24 - i * 8)) as u8); } // BE
                    println!("[{a:#08X}]={v:#010X}");
                }
            }
            "force" => {
                if let (Some(a), Some(v)) = (args.first().and_then(|s| parse_num(s)), args.get(1).and_then(|s| parse_num(s))) {
                    emu.force_read(a, v as u8); println!("force {a:#08X}=>{:#04X}", v as u8);
                }
            }
            "forcepc" => {
                // forcepc PC ADDR VAL - wymus odczyt ADDR=VAL tylko gdy CPU na PC.
                if let (Some(pc), Some(a), Some(v)) = (args.first().and_then(|s| parse_num(s)), args.get(1).and_then(|s| parse_num(s)), args.get(2).and_then(|s| parse_num(s))) {
                    emu.force_read_at(pc, a, v as u8); println!("forcepc @{pc:#08X}: {a:#08X}=>{:#04X}", v as u8);
                } else { println!("err: forcepc PC ADDR VAL"); }
            }
            "unforce" => { if let Some(a) = args.first().and_then(|s| parse_num(s)) { emu.unforce_read(a); println!("unforce {a:#08X}"); } }
            "clearf" => { emu.clear_forces(); println!("forces wyczyszczone"); }
            "key" => {
                if let Some(k) = args.first().and_then(|s| keycode(s)) {
                    emu.set_key(k, true); emu.run_steps(300_000); emu.set_key(k, false); emu.run_steps(300_000);
                    println!("[key {}] pc={:#08X}", args[0], emu.pc());
                } else { println!("err: key NAME"); }
            }
            "keydown" => { if let Some(k) = args.first().and_then(|s| keycode(s)) { emu.set_key(k, true); } }
            "keyup" => { if let Some(k) = args.first().and_then(|s| keycode(s)) { emu.set_key(k, false); } }
            "pin" => {
                if let Some(code) = args.first() {
                    for ch in code.chars().filter(|c| c.is_ascii_digit()) {
                        let d = ch.to_digit(10).unwrap() as u8;
                        emu.set_key(EmuKey::Digit(d), true); emu.run_steps(800_000);
                        emu.set_key(EmuKey::Digit(d), false); emu.run_steps(800_000);
                    }
                    emu.set_key(EmuKey::Select, true); emu.run_steps(800_000);
                    emu.set_key(EmuKey::Select, false);
                    // Zatrzymaj NA werdykcie: reject 0x2b0540 lub accept (kaskada) 0x2726be.
                    let (hit, done) = emu.run_until_any(&[0x002B_0540, 0x0027_26BE], 25_000_000);
                    let v = match hit { Some(0x002B_0540) => "REJECT(0x2b0540)", Some(0x0027_26BE) => "ACCEPT(0x2726be)", _ => "BRAK/MAX" };
                    println!("[pin {code}] WERDYKT={v} po {done} krokach, pc={:#08X}", emu.pc());
                }
            }
            "pinkeys" => {
                // Wpisz PIN (cyfry+select) BEZ lapania werdyktu - kontrola nad reszta recznie.
                if let Some(code) = args.first() {
                    for ch in code.chars().filter(|c| c.is_ascii_digit()) {
                        let d = ch.to_digit(10).unwrap() as u8;
                        emu.set_key(EmuKey::Digit(d), true); emu.run_steps(800_000);
                        emu.set_key(EmuKey::Digit(d), false); emu.run_steps(800_000);
                    }
                    emu.set_key(EmuKey::Select, true); emu.run_steps(800_000);
                    emu.set_key(EmuKey::Select, false); emu.run_steps(200_000);
                    println!("[pinkeys {code}] pc={:#08X}", emu.pc());
                }
            }
            "msgsrc" => {
                // Znajdz dostawe komunikatu SIM-MMI z data=0xA3 (reject) @0x2E96DA, zrzuc r0 (bufor zrodlowy).
                let want = args.first().and_then(|s| parse_num(s)).unwrap_or(0xA3);
                let mut found = false;
                for _ in 0..400 {
                    let (hit, _) = emu.run_until(0x002E_96DA, 3_000_000);
                    if !hit { break; }
                    let r0 = emu.get_reg(0);
                    let dataw = emu.read32(r0 + 4);
                    if dataw == want {
                        println!("[msgsrc] ZNALEZIONO data={want:#X} r0(bufor)={r0:#010X} [r0+8]={:#X} kroki={}", emu.read32(r0 + 8), emu.total_steps());
                        found = true; break;
                    }
                    emu.run_steps(2); // przejdz za breakpoint
                }
                if !found { println!("[msgsrc] nie znaleziono data={want:#X}"); }
            }
            "untilreg" => {
                // untilreg ADDR REGIDX VAL [MAXITER] - zatrzymaj gdy PC==ADDR i reg[REGIDX]==VAL; raportuj lr.
                let a = args.first().and_then(|s| parse_num(s));
                let ri = args.get(1).and_then(|s| parse_num(s)).map(|v| v as usize);
                let val = args.get(2).and_then(|s| parse_num(s));
                let maxit = args.get(3).and_then(|s| parse_num(s)).unwrap_or(2000);
                if let (Some(a), Some(ri), Some(val)) = (a, ri, val) {
                    let mut found = false;
                    for _ in 0..maxit {
                        let (hit, _) = emu.run_until(a, 3_000_000);
                        if !hit { break; }
                        if emu.get_reg(ri) == val {
                            println!("[untilreg] PC={a:#08X} r{ri}={val:#X} -> lr=r14={:#08X} (wolajacy) kroki={}", emu.get_reg(14), emu.total_steps());
                            for r in 0..16 { print!("r{r:<2}={:08X} ", emu.get_reg(r)); if r%4==3 { println!(); } }
                            found = true; break;
                        }
                        emu.run_steps(2);
                    }
                    if !found { println!("[untilreg] nie trafiono r{ri}={val:#X} @ {a:#08X}"); }
                } else { println!("err: untilreg ADDR REGIDX VAL"); }
            }
            "fill" => {
                if let (Some(a), Some(n), Some(v)) = (args.first().and_then(|s| parse_num(s)), args.get(1).and_then(|s| parse_num(s)), args.get(2).and_then(|s| parse_num(s))) {
                    for i in 0..n { emu.write8(a + i, v as u8); }
                    println!("[fill {a:#08X}..+{n}]={:#04X}", v as u8);
                }
            }
            "screen" | "lcd" => dump_screen(&emu),
            _ => println!("?? nieznana komenda: {cmd}"),
        }
    }
}
