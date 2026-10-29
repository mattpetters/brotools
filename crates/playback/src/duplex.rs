//! One clock for simultaneous record and playback.
//!
//! cpal opens input and output as two streams. When both use the same device, sample rate and
//! buffer, they share that device's clock. The record offset is the driver's input latency plus
//! its output latency, so a looped-back take lands on the timeline of what was heard. When the
//! driver reports neither latency, a fixed buffer counts once in each direction.
//!
//! The backlog trim on the monitor ring follows the approach in upstream
//! [soundcraft#808](https://github.com/storytold/soundcraft/pull/808) (Robbie). Named I/O paths
//! follow the model in [soundcraft#575](https://github.com/storytold/soundcraft/pull/575)
//! (Testsubject1337), without that change's channel-grid window. Low Latency Monitoring bypasses
//! inserts on monitored tracks, the idea in [soundcraft#206](https://github.com/storytold/soundcraft/pull/206)
//! (Jafz2001), without reopening the device at a second buffer size.

use crate::AudioConfig;

/// Samples to move a take earlier so it lines up with what was playing.
///
/// `user_offset` is added afterwards (positive still earlier). `compensate` false keeps only the
/// user offset. A reported latency of zero with a fixed `buffer_frames` uses two buffers, one
/// for the input callback and one for the output callback.
pub fn record_offset_samples(input_latency: u32, output_latency: u32, buffer_frames: u32, user_offset: i64, compensate: bool) -> i64 {
    if !compensate {
        return user_offset;
    }
    let reported = i64::from(input_latency).saturating_add(i64::from(output_latency));
    let auto = if reported > 0 { reported } else { i64::from(buffer_frames).saturating_mul(2) };
    user_offset.saturating_add(auto)
}

/// The input device to open for a full-duplex session: the named input, or the output device
/// when the input was left on the default and the output device is real.
pub fn input_device_name(explicit: Option<&str>, output_device: &str, output_silent: bool) -> Option<String> {
    if let Some(name) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(name.to_string());
    }
    if output_silent || output_device.trim().is_empty() {
        return None;
    }
    Some(output_device.to_string())
}

/// Whether `cfg` is asking for the output device's input side.
pub fn wants_same_device(cfg: &AudioConfig) -> bool {
    match cfg.input.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => true,
        Some(input) => cfg.output.as_deref().is_some_and(|output| input.eq_ignore_ascii_case(output.trim())),
    }
}

/// Run `input` and `output` in lockstep, `buffer_frames` at a time, one channel.
/// `render` reads this buffer's input and writes the same number of output frames.
/// Both directions advance by the same frame count: one clock.
pub fn run_duplex(buffer_frames: u32, input: &[f32], mut render: impl FnMut(&[f32], &mut [f32])) -> Vec<f32> {
    let buf = usize::try_from(buffer_frames).unwrap_or(1).max(1);
    let mut out = vec![0.0; input.len()];
    let mut at = 0;
    while at < input.len() {
        let n = buf.min(input.len() - at);
        let Some(inp) = input.get(at..at + n) else { break };
        let Some(dst) = out.get_mut(at..at + n) else { break };
        render(inp, dst);
        at += n;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplex_callback_plays_the_input_from_the_same_buffer() {
        let input: Vec<f32> = (0..256).map(|i| if i == 10 || i == 40 { 0.5 } else { 0.0 }).collect();
        let out = run_duplex(32, &input, |inp, dst| dst.copy_from_slice(inp));
        assert_eq!(out, input);
        assert_eq!(out.len(), input.len());
    }

    #[test]
    fn record_offset_adds_both_directions_and_the_user_trim() {
        assert_eq!(record_offset_samples(64, 128, 32, 0, true), 192);
        assert_eq!(record_offset_samples(0, 0, 128, 0, true), 256);
        assert_eq!(record_offset_samples(0, 0, 128, -10, true), 246);
        assert_eq!(record_offset_samples(64, 128, 32, 5, false), 5);
        assert_eq!(record_offset_samples(0, 0, 0, 5, true), 5);
    }

    #[test]
    fn an_unset_input_follows_the_output_device() {
        assert_eq!(input_device_name(None, "Interface", false).as_deref(), Some("Interface"));
        assert_eq!(input_device_name(Some("Mic"), "Interface", false).as_deref(), Some("Mic"));
        assert_eq!(input_device_name(None, "Interface", true), None);
        let cfg = AudioConfig { output: Some("Interface".into()), ..AudioConfig::default() };
        assert!(wants_same_device(&cfg));
        let split = AudioConfig { output: Some("Interface".into()), input: Some("Mic".into()), ..AudioConfig::default() };
        assert!(!wants_same_device(&split));
    }
}
