//! Wyjscie audio buzzera: synteza fali prostokatnej (jak sprzetowy buzzer 3310) przez cpal.
//! Stan (czest./glosnosc/gate) ustawiany z petli GUI co klatke przez atomiki; callback audio
//! (osobny watek) czyta je bez blokad i generuje probki.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

struct Shared {
    freq: AtomicU32,    // Hz
    vol: AtomicU32,     // 0..255
    playing: AtomicBool,
}

pub struct Buzzer {
    shared: Arc<Shared>,
    _stream: cpal::Stream, // utrzymuje strumien przy zyciu
}

impl Buzzer {
    /// Inicjalizuje wyjscie audio. None gdy brak urzadzenia / nieobslugiwany format (gra bez dzwieku).
    pub fn new() -> Option<Self> {
        let host = cpal::default_host();
        let device = host.default_output_device()?;
        let config = device.default_output_config().ok()?;
        let sample_rate = config.sample_rate().0 as f32;
        let channels = config.channels() as usize;
        let fmt = config.sample_format();

        let shared = Arc::new(Shared {
            freq: AtomicU32::new(0),
            vol: AtomicU32::new(0),
            playing: AtomicBool::new(false),
        });
        let s = shared.clone();
        let mut phase = 0.0f32; // 0..1 w obrebie okresu
        let err_fn = |e| eprintln!("[audio] blad strumienia: {e}");

        // Generator fali prostokatnej do bufora f32 (mono powielone na kanaly).
        let gen = move |data: &mut [f32]| {
            let playing = s.playing.load(Ordering::Relaxed);
            let freq = s.freq.load(Ordering::Relaxed) as f32;
            let vol = s.vol.load(Ordering::Relaxed) as f32 / 255.0;
            let amp = if playing && freq > 0.0 { vol * 0.18 } else { 0.0 };
            let step = if freq > 0.0 { freq / sample_rate } else { 0.0 };
            for frame in data.chunks_mut(channels.max(1)) {
                let sample = if amp > 0.0 {
                    if phase < 0.5 { amp } else { -amp }
                } else {
                    0.0
                };
                for ch in frame.iter_mut() {
                    *ch = sample;
                }
                phase += step;
                if phase >= 1.0 {
                    phase -= 1.0;
                }
            }
        };

        // cpal wymaga osobnego callbacka per format probki - obslugujemy najczestsze.
        let cfg: cpal::StreamConfig = config.into();
        let stream = match fmt {
            cpal::SampleFormat::F32 => {
                let mut g = gen;
                device.build_output_stream(&cfg, move |d: &mut [f32], _| g(d), err_fn, None)
            }
            cpal::SampleFormat::I16 => {
                let mut g = gen;
                let mut buf = Vec::<f32>::new();
                device.build_output_stream(
                    &cfg,
                    move |d: &mut [i16], _| {
                        buf.resize(d.len(), 0.0);
                        g(&mut buf);
                        for (o, &v) in d.iter_mut().zip(buf.iter()) {
                            *o = (v.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                        }
                    },
                    err_fn,
                    None,
                )
            }
            cpal::SampleFormat::U16 => {
                let mut g = gen;
                let mut buf = Vec::<f32>::new();
                device.build_output_stream(
                    &cfg,
                    move |d: &mut [u16], _| {
                        buf.resize(d.len(), 0.0);
                        g(&mut buf);
                        for (o, &v) in d.iter_mut().zip(buf.iter()) {
                            *o = (((v.clamp(-1.0, 1.0) + 1.0) * 0.5) * u16::MAX as f32) as u16;
                        }
                    },
                    err_fn,
                    None,
                )
            }
            _ => return None,
        }
        .ok()?;

        stream.play().ok()?;
        Some(Self { shared, _stream: stream })
    }

    /// Ustaw stan buzzera (wolane co klatke GUI). freq w Hz, vol 0..255.
    pub fn update(&self, freq: u32, vol: u8, playing: bool) {
        self.shared.freq.store(freq, Ordering::Relaxed);
        self.shared.vol.store(vol as u32, Ordering::Relaxed);
        self.shared.playing.store(playing, Ordering::Relaxed);
    }
}
