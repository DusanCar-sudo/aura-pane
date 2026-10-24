//! Music-reactive edge. While any app is playing, tap the default sink's monitor
//! and emit a bass-weighted 0..1 beat level ~30x/s (`aura://beat`), plus
//! `aura://music` true/false so the edge swaps breathing for the beat.

use serde_json::Value;
use std::io::{self, Read};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

const RATE: usize = 8000;
const FRAME: usize = RATE / 30; // ~33ms per level
const POLL: Duration = Duration::from_secs(2);
/// One-pole low-pass at ~150Hz (8kHz rate): lets the kick drum drive the edge.
const LP_ALPHA: f32 = 0.105;
const PEAK_DECAY: f32 = 0.98; // auto-gain falls ~45%/s after a loud hit
const AVG_ALPHA: f32 = 0.1; // ~0.3s running loudness, onsets are measured against it
const FLOOR: f32 = 0.002; // below this RMS counts as silence
const QUIET_FRAMES: u32 = 60; // 2s of silence ends "music"

#[derive(Default)]
struct Meter {
    lp: f32,
    peak: f32,
    avg: f32,
}

impl Meter {
    /// One frame of samples in -1..1 → beat level 0..1 (0 = silence).
    fn push(&mut self, frame: &[f32]) -> f32 {
        let (mut bass, mut full) = (0.0, 0.0);
        for &x in frame {
            self.lp += LP_ALPHA * (x - self.lp);
            bass += self.lp * self.lp;
            full += x * x;
        }
        let n = frame.len().max(1) as f32;
        let rms = 0.7 * (bass / n).sqrt() + 0.3 * (full / n).sqrt();
        self.peak = rms.max(self.peak * PEAK_DECAY).max(FLOOR);
        // Mostly onset (jump above the running level), a little absolute loudness.
        let onset = ((rms - self.avg) / (self.peak - self.avg).max(1e-4)).clamp(0.0, 1.0);
        self.avg += AVG_ALPHA * (rms - self.avg);
        if rms < FLOOR {
            0.0
        } else {
            0.7 * onset + 0.3 * rms / self.peak
        }
    }
}

/// Any uncorked (actually playing) stream on the sound server?
fn playing() -> bool {
    Command::new("pactl")
        .args(["-f", "json", "list", "sink-inputs"])
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
        .and_then(|v| v.as_array().map(|a| a.iter().any(|s| s["corked"] == false)))
        .unwrap_or(false)
}

pub fn watch(app: AppHandle) {
    std::thread::spawn(move || loop {
        if playing() {
            if let Err(e) = tap(&app) {
                eprintln!("aura-pane: music tap: {e}");
            }
        }
        std::thread::sleep(POLL);
    });
}

fn tap(app: &AppHandle) -> io::Result<()> {
    let mut child = Command::new("parec")
        .args(["-d", "@DEFAULT_MONITOR@", "--format=s16le", "--rate=8000", "--channels=1", "--latency-msec=30"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let result = pump(app, &mut child);
    let _ = child.kill();
    let _ = child.wait();
    let _ = app.emit("aura://music", false);
    result
}

/// Reads until playback stops; the monitor capture is dropped so the sink can suspend.
fn pump(app: &AppHandle, child: &mut Child) -> io::Result<()> {
    let mut out = child.stdout.take().ok_or_else(|| io::Error::other("parec has no stdout"))?;
    let mut buf = [0u8; FRAME * 2];
    let mut frame = [0f32; FRAME];
    let mut meter = Meter::default();
    let (mut on, mut quiet, mut checked) = (false, 0u32, Instant::now());
    loop {
        out.read_exact(&mut buf)?;
        for (s, b) in frame.iter_mut().zip(buf.chunks_exact(2)) {
            *s = f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0;
        }
        let level = meter.push(&frame);
        quiet = if level > 0.0 { 0 } else { quiet + 1 };
        if !on && level > 0.0 {
            on = true;
            let _ = app.emit("aura://music", true);
        } else if on && quiet >= QUIET_FRAMES {
            on = false;
            let _ = app.emit("aura://music", false);
        }
        if on {
            let _ = app.emit("aura://beat", level);
        }
        if checked.elapsed() >= POLL {
            if !playing() {
                return Ok(());
            }
            checked = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hz: f32, amp: f32, start: usize) -> Vec<f32> {
        (start..start + FRAME)
            .map(|i| amp * (std::f32::consts::TAU * hz * i as f32 / RATE as f32).sin())
            .collect()
    }

    #[test]
    fn silence_is_zero() {
        let mut m = Meter::default();
        assert_eq!(m.push(&[0.0; FRAME]), 0.0);
    }

    #[test]
    fn kick_drum_peaks_and_falls_between_beats() {
        // 120 bpm: a 60Hz kick every 15 frames over a quiet 1kHz bed.
        let mut m = Meter::default();
        let (mut on_beat, mut between) = (Vec::new(), Vec::new());
        for f in 0..150 {
            let mut s = tone(1000.0, 0.05, f * FRAME);
            if f % 15 < 2 {
                s.iter_mut().zip(tone(60.0, 0.8, f * FRAME)).for_each(|(a, k)| *a += k);
            }
            let level = m.push(&s);
            if f >= 30 && f % 15 == 0 {
                on_beat.push(level);
            }
            if f >= 30 && f % 15 == 8 {
                between.push(level);
            }
        }
        assert!(on_beat.iter().all(|&l| l > 0.7), "beats {on_beat:?}");
        assert!(between.iter().all(|&l| l < 0.35), "between {between:?}");
    }
}
