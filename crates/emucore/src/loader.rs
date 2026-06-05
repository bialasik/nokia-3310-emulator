//! Loader ROM (M0): sklada surowe zrzuty regionow .fls w jeden obraz pamieci
//! flasha 0x000000..0x400000 (4 MB). Wg analizy dostarczonych plikow:
//!   - 0x1D0000 B  -> firmware + PPM, baza 0x200000
//!   - 0x30000  B  -> EEPROM,        baza 0x3D0000
//! Niewypelnione obszary = 0xFF (jak skasowany flash).

use std::fs;
use std::path::{Path, PathBuf};

/// Rozmiar przestrzeni adresowej flasha (4 MB).
pub const IMAGE_SIZE: usize = 0x0040_0000;
/// Baza regionu firmware+PPM.
pub const FLASH_BASE: u32 = 0x0020_0000;
/// Baza regionu EEPROM.
pub const EEPROM_BASE: u32 = 0x003D_0000;

/// Oczekiwane rozmiary regionow (do autodetekcji po rozmiarze pliku).
const SZ_FIRMWARE: u64 = 0x1D_0000; // 0x200000..0x3D0000
const SZ_EEPROM: u64 = 0x3_0000; // 0x3D0000..0x400000

/// Katalogi przeszukiwane w poszukiwaniu plikow ROM (wzgledem cwd).
const ROM_DIRS: &[&str] = &["roms", "crates/rom"];

/// Zlozony obraz pamieci + metadane.
pub struct Rom {
    pub mem: Vec<u8>,
    pub firmware_id: String,
    pub loaded: Vec<LoadedRegion>,
}

pub struct LoadedRegion {
    pub name: String,
    pub base: u32,
    pub size: u64,
    pub file: PathBuf,
}

impl Rom {
    /// Czyta bajt z obrazu (adres absolutny w przestrzeni flasha).
    #[allow(dead_code)]
    pub fn read8(&self, addr: u32) -> u8 {
        self.mem.get(addr as usize).copied().unwrap_or(0xFF)
    }
}

/// Wczytuje i sklada obraz ROM. `Ok(None)` = nie znaleziono zadnych plikow.
pub fn load() -> Result<Option<Rom>, String> {
    // Tryb testowy: FW_FILE=<plik> -> wczytaj go bezposrednio na 0x200000 (np. MADos).
    if let Ok(path) = std::env::var("FW_FILE") {
        let data = fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
        let mut mem = vec![0xFFu8; IMAGE_SIZE];
        let start = FLASH_BASE as usize;
        let end = (start + data.len()).min(IMAGE_SIZE);
        mem[start..end].copy_from_slice(&data[..end - start]);
        let mut loaded = vec![LoadedRegion {
            name: "FW_FILE".into(),
            base: FLASH_BASE,
            size: data.len() as u64,
            file: PathBuf::from(&path),
        }];
        // Doladuj EEPROM: jawny EEPROM_FILE, albo auto-skan pliku rozmiaru SZ_EEPROM.
        let eeprom = std::env::var("EEPROM_FILE").ok().map(PathBuf::from).or_else(|| {
            scan_rom_files().into_iter().find(|p| {
                fs::metadata(p).map(|m| m.len() == SZ_EEPROM).unwrap_or(false)
            })
        });
        if let Some(ep) = eeprom {
            if let Ok(ed) = fs::read(&ep) {
                let s = EEPROM_BASE as usize;
                let e = (s + ed.len()).min(IMAGE_SIZE);
                mem[s..e].copy_from_slice(&ed[..e - s]);
                loaded.push(LoadedRegion { name: "EEPROM".into(), base: EEPROM_BASE, size: ed.len() as u64, file: ep });
            }
        }
        let firmware_id = extract_firmware_id(&mem);
        return Ok(Some(Rom {
            mem,
            firmware_id: format!("FW_FILE {firmware_id}"),
            loaded,
        }));
    }

    let files = scan_rom_files();
    if files.is_empty() {
        return Ok(None);
    }

    let mut mem = vec![0xFFu8; IMAGE_SIZE];
    let mut loaded = Vec::new();

    for path in files {
        let data = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let size = data.len() as u64;

        // Wyznacz baze: 1) z nazwy pliku (token 8-hex), 2) z rozmiaru.
        let (base, name) = if let Some(b) = base_from_name(&path) {
            (b, "z nazwy")
        } else {
            match size {
                SZ_FIRMWARE => (FLASH_BASE, "firmware+PPM"),
                SZ_EEPROM => (EEPROM_BASE, "EEPROM"),
                _ => {
                    eprintln!(
                        "[rom] pomijam {} (nieznany rozmiar {} B, brak adresu w nazwie)",
                        path.display(),
                        size
                    );
                    continue;
                }
            }
        };

        let start = base as usize;
        let end = start + data.len();
        if end > mem.len() {
            return Err(format!(
                "{}: region {start:#08X}..{end:#08X} wychodzi poza obraz {IMAGE_SIZE:#08X}",
                path.display()
            ));
        }
        mem[start..end].copy_from_slice(&data);
        loaded.push(LoadedRegion {
            name: name.to_string(),
            base,
            size,
            file: path,
        });
    }

    let firmware_id = extract_firmware_id(&mem);
    Ok(Some(Rom {
        mem,
        firmware_id,
        loaded,
    }))
}

/// Zbiera kandydujace pliki ROM z katalogow ROM_DIRS (.fls/.bin), wieksze najpierw.
fn scan_rom_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in ROM_DIRS {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if !p.is_file() {
                continue;
            }
            let ext = p
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.to_ascii_lowercase());
            if matches!(ext.as_deref(), Some("fls") | Some("bin")) {
                out.push(p);
            }
        }
    }
    // Wieksze pliki najpierw (firmware przed eeprom) - stabilne mapowanie.
    out.sort_by_key(|p| std::cmp::Reverse(fs::metadata(p).map(|m| m.len()).unwrap_or(0)));
    out
}

/// Probuje wyciagnac bazowy adres z nazwy pliku (token 8 cyfr hex, np. "003D0000").
fn base_from_name(path: &Path) -> Option<u32> {
    let stem = path.file_stem()?.to_str()?;
    // Szukaj ciaglego ciagu 8 znakow hex.
    let bytes: Vec<char> = stem.chars().collect();
    for w in bytes.windows(8) {
        if w.iter().all(|c| c.is_ascii_hexdigit()) {
            let s: String = w.iter().collect();
            if let Ok(v) = u32::from_str_radix(&s, 16) {
                // Sensowny zakres flasha.
                if (v as usize) < IMAGE_SIZE {
                    return Some(v);
                }
            }
        }
    }
    None
}

/// Wyciaga string identyfikacyjny firmware z okolic 0x200200 (NHM-5 / wersja).
fn extract_firmware_id(mem: &[u8]) -> String {
    let start = 0x0020_0200usize;
    let slice = mem.get(start..start + 64).unwrap_or(&[]);
    // Zamien bajty niedrukowalne na '\n' separator, sklej drukowalne linie.
    let text: String = slice
        .iter()
        .map(|&b| if (0x20..0x7F).contains(&b) { b as char } else { '\n' })
        .collect();
    let mut parts: Vec<&str> = text
        .split('\n')
        // przytnij wiodace/koncowe znaki niealfanumeryczne (np. ".33" -> "33")
        .map(|s| s.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|s| !s.is_empty())
        .collect();
    parts.truncate(3); // np. ["33", "33-DM-01", "NHM-5"]
    if parts.is_empty() {
        "?".to_string()
    } else {
        parts.join(" ")
    }
}

/// Wypisuje na stdout mape zlozonego obrazu.
pub fn print_map(rom: &Rom) {
    println!("=== ROM zaladowany ===");
    println!("firmware id : {}", rom.firmware_id);
    println!("obraz       : {:#08X} B (4 MB)", IMAGE_SIZE);
    for r in &rom.loaded {
        println!(
            "  {:>12}  {:#08X}..{:#08X}  ({} B)  <- {}",
            r.name,
            r.base,
            r.base as u64 + r.size,
            r.size,
            r.file.display()
        );
    }
    println!("  reszta = 0xFF (skasowany flash)");
}
