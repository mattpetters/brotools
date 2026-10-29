# Hardware parity

> **Last reviewed:** 2026-10-29 · **Last updated:** 2026-10-29 · **Change:** major (MIDI devices; same-device duplex, record offset, low-latency monitoring) · **Target:** Avid Pro Tools Ultimate 2026.4.1

A DAW is judged by how it drives audio interfaces, MIDI gear, control surfaces and sync. This is
SoundCraft's weakest dimension. Ours is measured from `crates/playback` (cpal, and midir for MIDI
ports) and the absence of control-surface code; Pro Tools' from its documentation and Setup menus.

**Hardware: ~12 % ready** (estimated). **Performance: ~35 %** (estimated; nothing benchmarked
against Pro Tools). Remaining ≈ 110–180 h for hardware, 40–70 h for performance.

Legend: ✅ works · 🟡 partial · ❌ missing.

## Audio interfaces

| Feature | Pro Tools (macOS / Windows) | SoundCraft macOS | Windows | Linux | FreeBSD | Web |
|---|---|---|---|---|---|---|
| Driver API | Core Audio / ASIO | 🟡 Core Audio via cpal | 🟡 WASAPI shared via cpal; ASIO with the opt-in `asio` build feature (verified with an SSL 2+) | 🟡 ALSA/Pulse via cpal; no JACK | 🟡 cpal (ALSA layer) | 🟡 Web Audio |
| Choose playback device | ✅ Playback Engine | 🟡 Setup › Playback Engine (not yet verified on macOS) | ✅ Setup › Playback Engine (WASAPI, ASIO) | 🟡 (not yet verified) | 🟡 (not yet verified) | ❌ |
| Choose input device | ✅ | 🟡 Setup › Playback Engine (not yet verified on macOS) | ✅ (WASAPI, ASIO) | 🟡 (not yet verified) | ❌ no recording | ❌ |
| Buffer size (32–2048) | ✅ | 🟡 device buffer when the driver allows it (not yet verified); mix in 512-sample blocks | 🟡 device buffer 16–8192 (verified 128 on ASIO, 256 on WASAPI); mix in 512-sample blocks | 🟡 (not yet verified) | 🟡 | ❌ |
| Duplex stream on one clock | ✅ | 🟡 same device, rate and buffer; two cpal callbacks | 🟡 | 🟡 | 🟡 | ❌ |
| Hardware I/O mapping (I/O Setup to physical channels) | ✅ | 🟡 named paths and channel lists, per track | 🟡 | 🟡 | 🟡 | 🟡 |
| Low-latency monitoring | ✅ | 🟡 backlog trim; LLM bypasses inserts and PDC on monitored tracks | 🟡 | 🟡 | 🟡 | 🟡 |
| Latency reporting and record offset compensation | ✅ | 🟡 driver timestamps, else two buffers, plus a user offset | 🟡 | 🟡 | 🟡 | 🟡 |
| Sample rate follows the session; resample when it can't | ✅ (switches device) | 🟡 resamples (input pitch bug fixed, #28) | 🟡 | 🟡 | 🟡 | 🟡 |
| Avid HDX / HD Native DSP, Hybrid Engine DSP mode | ✅ | ❌ (out of reach: Avid hardware) | ❌ | ❌ | ❌ | ❌ |
| Disk allocation / record to disk | ✅ | 🟡 records to WAV (upstream #84); playback still whole-file RAM | 🟡 | 🟡 | ❌ no recording | ❌ |

## MIDI

| Feature | Pro Tools | SoundCraft (all platforms) |
|---|---|---|
| MIDI input devices (keyboards, pads) | ✅ | 🟡 CoreMIDI / WinMM / ALSA via midir; hot-plug; per-track port and channel. Not yet played on a physical keyboard on macOS or Windows |
| MIDI output to external gear | ✅ | 🟡 per-track port and channel; clip playback and thru. Same verification limit |
| MIDI Thru, input filter | ✅ | 🟡 thru to armed, input-monitored, or (option on) the selected track; the input filter is applied on the way in |
| MIDI Beat Clock out, MTC generate/chase | ✅ | 🟡 clock (Start/Continue/Stop + 24 ppqn) and quarter-frame MTC out at 24/25/29.97/30. No chase |
| MIDI learn / controller mapping | ✅ (via surfaces and plug-in maps) | ❌ |
| Virtual MIDI keyboard | ✅ (MIDI Keyboard window) | 🟡 window exists; it does not feed the hardware recorder |

## Control surfaces

| Feature | Pro Tools | SoundCraft |
|---|---|---|
| EUCON (S1/S3/S4/S6, Dock, Control app) | ✅ | ❌ Avid-proprietary SDK; owner decision |
| HUI | ✅ | ❌ |
| Mackie Control Universal | ✅ (via HUI emulation and personalities) | ❌ |
| Generic MIDI controller personalities | ✅ | ❌ |

## Sync and video hardware

| Feature | Pro Tools | SoundCraft |
|---|---|---|
| LTC / MTC chase, 9-pin machine control | ✅ (Ultimate) | ❌ |
| Word clock / video reference awareness | ✅ via interface | ❌ |
| Video output to a second display or video card | ✅ | ❌ (video window only) |
| Satellite linking of systems | ✅ | ❌ |

## Performance

| Feature | Pro Tools | SoundCraft | Evidence |
|---|---|---|---|
| Parallel mixing across cores | ✅ | ✅ | `soundcraft-mix` renders independent strips in parallel (rayon) |
| Plugin delay compensation | ✅ | ✅ | Bounces of a bus that an aux returns onto are compensated like the main mix (#246). |
| Disk streaming of long sources | ✅ | ❌ | Whole files decoded into the `SourcePool` |
| Track count (Ultimate: 2,048 voices) | ✅ | unmeasured | No benchmark exists; add `cargo xtask bench` |
| Real-time audio thread discipline | ✅ | ✅ | No blocking/allocation in steady state, audio-thread-aware logger |
| Freeze / commit to save CPU | ✅ | 🟡 | `track.commit` renders; `track.freeze` only sets a flag (no CPU saved) |
| GPU-accelerated UI | — | ✅ (wgpu/glow via eframe) | llvmpipe X11 issue #107 |

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-29 | major | MIDI input and output devices, live play-through, recording, beat clock and MTC out |
| 2026-10-29 | minor | Same-device duplex clock, named input/output channel maps, record-offset compensation, low-latency monitoring. Record-to-disk was already in tree |
| 2026-10-13 | trivial | bounces of a bus fed by an aux compensate its latency (#246) |
| 2026-10-12 | minor | Audio device settings: system, output, input, sample rate and buffer size; opt-in ASIO |
| 2026-10-10 | major | Created: audit of audio interface, MIDI, control-surface, sync and performance support against Pro Tools 2026.4.1 |
