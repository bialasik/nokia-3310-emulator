//! Rdzen emulacji Nokia 3310 (DCT3).
//! - `loader`  - skladanie obrazu flasha 0x000000..0x400000 z plikow .fls
//! - `machine` - magistrala (MemoryInterface) z routingiem flash/scratch + tracer MMIO

pub mod buzzer;
pub mod ccont;
pub mod ctsi;
pub mod dsp;
pub mod emu;
pub mod flash;
pub mod keypad;
pub mod lcd;
pub mod loader;
pub mod machine;
pub mod mbus;
pub mod periph;
pub mod sim;

pub use emu::{EmuKey, Emulator};
pub use lcd::Pcd8544;
pub use loader::Rom;
pub use machine::Machine;
