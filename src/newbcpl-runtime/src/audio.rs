//! BCPL `Sound_*` and `Music_*` runtime — game-focused SFX and ABC
//! music playback, backed by NewAudio (`E:\NewAudio\NewAudio\`).
//!
//! This is the BCPL twin of `newfb-runtime/src/audio.rs`. NewFB wraps
//! NewAudio behind BASIC's `SOUND COIN 1, ...` / `MUSIC LOAD 1, "..."`
//! namespace-statement idiom; BCPL has no compound statements, so
//! every operation is just a function call here. The user-facing
//! surface in `modules-active/audio.bcl` re-exports these under
//! short names (`audio_coin`, `audio_play`, ...) the same way
//! `igui.bcl` wraps the `iGui_*` builtins.
//!
//! NewAudio is the upstream audio crate family:
//!
//! - `newaudio-core` — platform-independent synthesis: preset SFX
//!   (`coin`, `jump`, `explode`, …), tone / noise / FM builders, ADSR,
//!   WAV reader/writer.
//! - `newaudio-abc`  — platform-independent ABC notation parser and
//!   Standard MIDI File writer.
//! - `newaudio-win`  — Windows runtime: `waveOut` PCM mixer with
//!   per-voice gain/pan and a `midiOut` scheduler that consumes parsed
//!   `AbcTune`s. Compiled in only on Windows.
//!
//! ## Slot model
//!
//! BCPL users name sounds by an integer slot — `Sound_Coin(1, 1.2, 0.1)`
//! puts a coin SFX at slot 1; `Sound_Play(1, 1.0, 0.0)` plays it.
//! NewAudio assigns its own `SoundId` / `MidiAssetId` internally; the
//! slot maps here translate BCPL's slot to that id. Re-binding a slot
//! frees the previously-registered NewAudio asset first so memory does
//! not leak across `Sound_Coin(1, ...)` / `Sound_Zap(1, ...)` rebindings.
//!
//! ## ABI
//!
//! Slots and counts are `i64`; float parameters are `f64` — the
//! natural BCPL word widths. The engine internally works at f32 (audio
//! sample rate doesn't need double-precision); we cast at the FFI
//! boundary. Return codes are i64 too: 0 = OK, non-zero = error kind.
//!
//! Float arguments must arrive in XMM registers under the Win64
//! calling convention — the matching declarations in
//! `newbcpl-llvm::emit::declare_extern` spell out f64 explicitly for
//! every function below so the JIT doesn't accidentally route them
//! through integer registers.
//!
//! ## Cross-platform behaviour
//!
//! `newaudio-core` and `newaudio-abc` compile everywhere, so preset
//! synthesis and ABC parsing work on Linux/macOS too. The `waveOut`
//! and `midiOut` runtimes are Windows-only; on other targets every
//! `Sound_Play` / `Music_Play` reduces to a slot lookup with no
//! audible output. Slot bookkeeping and introspection still respond
//! consistently so unit tests can run on a CI Linux box.
//!
//! ## Lazy startup
//!
//! `Mixer::start` opens `waveOut` and `midiOut`. CI agents and some
//! headless containers don't have audio devices — we don't want
//! `newbcpl-driver run hello.bcl` to fail just because there's no
//! sound card. The mixer is started on the first Sound/Music play
//! call; if it fails, the error is remembered and subsequent plays
//! are silent no-ops (the slot bank still works for synthesis-only
//! operations).

#![allow(non_snake_case)]

use std::collections::HashMap;
use std::ffi::CStr;
use std::sync::{Mutex, OnceLock};

use newaudio_abc::AbcParser;
use newaudio_core::{Adsr, Buffer, Config, Engine, FilterType, NoiseType, Waveform};

#[cfg(windows)]
use newaudio_win::{MidiAssetId, Mixer, PlaybackState, SoundId};

#[cfg(not(windows))]
type SoundId = u32;
#[cfg(not(windows))]
type MidiAssetId = u32;

/// Returned by `Music_State`. Mirrors `newaudio_win::PlaybackState`
/// so callers don't need to depend on NewAudio enums.
pub const MUSIC_STATE_STOPPED: i64 = 0;
pub const MUSIC_STATE_PLAYING: i64 = 1;
pub const MUSIC_STATE_PAUSED: i64 = 2;

/// Returned by load/play shims to signal success / failure without
/// having to wedge a result type through the JIT FFI boundary.
pub const BCPL_AUDIO_OK: i64 = 0;
pub const BCPL_AUDIO_ERR_PARSE: i64 = 1;
pub const BCPL_AUDIO_ERR_UNKNOWN_SLOT: i64 = 2;
pub const BCPL_AUDIO_ERR_NO_DEVICE: i64 = 3;

struct SoundSlot {
    duration: f32,
    #[cfg(windows)]
    na_id: SoundId,
    #[cfg(not(windows))]
    _buffer: Buffer,
}

struct MusicSlot {
    /// Held for introspection (see `music_title_snapshot` in tests).
    /// Exposing the title over the FFI boundary would need a
    /// heap-string return convention BCPL doesn't have yet — the
    /// same staging note applies as in NewFB's audio.rs.
    #[allow(dead_code)]
    title: String,
    tempo_bpm: f32,
    #[cfg(windows)]
    na_id: MidiAssetId,
}

struct AudioState {
    engine: Engine,
    sound_slots: HashMap<i64, SoundSlot>,
    music_slots: HashMap<i64, MusicSlot>,
    sfx_volume: f32,
    music_volume: f32,
    #[cfg(windows)]
    mixer: Option<Mixer>,
    /// Set once we've tried to start the mixer; `false` here means
    /// startup failed and further attempts are skipped.
    #[cfg(windows)]
    mixer_tried: bool,
}

impl AudioState {
    fn new() -> Self {
        Self {
            engine: Engine::new(Config::default()),
            sound_slots: HashMap::new(),
            music_slots: HashMap::new(),
            sfx_volume: 1.0,
            music_volume: 1.0,
            #[cfg(windows)]
            mixer: None,
            #[cfg(windows)]
            mixer_tried: false,
        }
    }

    /// Lazily open `waveOut` / `midiOut`. Returns `Some(&Mixer)` if
    /// the runtime is up; `None` on a headless host where startup
    /// failed (and stays `None` for the rest of the process).
    #[cfg(windows)]
    fn mixer(&mut self) -> Option<&Mixer> {
        if !self.mixer_tried {
            self.mixer_tried = true;
            match Mixer::start() {
                Ok(m) => {
                    m.set_sfx_bus_volume(self.sfx_volume);
                    m.set_music_bus_volume(self.music_volume);
                    self.mixer = Some(m);
                }
                Err(_) => self.mixer = None,
            }
        }
        self.mixer.as_ref()
    }

    fn store_sound(&mut self, slot: i64, buf: Buffer) {
        let duration = buf.duration;
        self.free_sound(slot);
        #[cfg(windows)]
        let na_id = match self.mixer() {
            Some(m) => m.pcm().register_sound(buf),
            None => 0,
        };
        #[cfg(not(windows))]
        let _ = buf;
        self.sound_slots.insert(
            slot,
            SoundSlot {
                duration,
                #[cfg(windows)]
                na_id,
                #[cfg(not(windows))]
                _buffer: _stash_buffer(),
            },
        );
    }

    fn free_sound(&mut self, slot: i64) -> bool {
        let Some(old) = self.sound_slots.remove(&slot) else {
            return false;
        };
        #[cfg(windows)]
        if let Some(m) = self.mixer.as_ref()
            && old.na_id != 0
        {
            m.pcm().free_sound(old.na_id);
        }
        #[cfg(not(windows))]
        let _ = old;
        true
    }

    fn free_all_sounds(&mut self) {
        self.sound_slots.clear();
        #[cfg(windows)]
        if let Some(m) = self.mixer.as_ref() {
            m.pcm().free_all();
        }
    }

    fn free_music(&mut self, slot: i64) -> bool {
        let Some(old) = self.music_slots.remove(&slot) else {
            return false;
        };
        #[cfg(windows)]
        if let Some(m) = self.mixer.as_ref() {
            m.midi().free(old.na_id);
        }
        #[cfg(not(windows))]
        let _ = old;
        true
    }

    fn free_all_music(&mut self) {
        self.music_slots.clear();
        #[cfg(windows)]
        if let Some(m) = self.mixer.as_ref() {
            m.midi().free_all();
        }
    }
}

#[cfg(not(windows))]
fn _stash_buffer() -> Buffer {
    Buffer {
        sample_rate: 0,
        channels: 0,
        duration: 0.0,
        samples: Vec::new(),
    }
}

static STATE: OnceLock<Mutex<AudioState>> = OnceLock::new();

fn with_state<R>(f: impl FnOnce(&mut AudioState) -> R) -> R {
    let mu = STATE.get_or_init(|| Mutex::new(AudioState::new()));
    let mut guard = mu.lock().expect("audio state mutex poisoned");
    f(&mut guard)
}

// ─── Sound presets ────────────────────────────────────────────────
//
// Each `Sound_<Preset>(slot, p1, p2)` lowers to a single shim that
// renders the buffer with NewAudio's preset engine and stashes it in
// the slot bank. The two-parameter shape matches NewFB's reference
// (`docs/REFERENCE.md` § SOUND); BCPL programs pass float literals
// (`1.0` not `1`) so the Win64 ABI routes them through XMM.

#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_Beep(slot: i64, frequency: f64, duration: f64) -> i64 {
    with_state(|s| {
        let buf = s.engine.beep(frequency as f32, duration as f32);
        s.store_sound(slot, buf);
    });
    BCPL_AUDIO_OK
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_Coin(slot: i64, pitch: f64, duration: f64) -> i64 {
    with_state(|s| {
        let buf = s.engine.coin(pitch as f32, duration as f32);
        s.store_sound(slot, buf);
    });
    BCPL_AUDIO_OK
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_Jump(slot: i64, power: f64, duration: f64) -> i64 {
    with_state(|s| {
        let buf = s.engine.jump(power as f32, duration as f32);
        s.store_sound(slot, buf);
    });
    BCPL_AUDIO_OK
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_Explode(slot: i64, size: f64, duration: f64) -> i64 {
    with_state(|s| {
        let buf = s.engine.explode(size as f32, duration as f32);
        s.store_sound(slot, buf);
    });
    BCPL_AUDIO_OK
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_BigExplode(slot: i64, size: f64, duration: f64) -> i64 {
    with_state(|s| {
        let buf = s.engine.big_explosion(size as f32, duration as f32);
        s.store_sound(slot, buf);
    });
    BCPL_AUDIO_OK
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_SmallExplode(slot: i64, intensity: f64, duration: f64) -> i64 {
    with_state(|s| {
        let buf = s.engine.small_explosion(intensity as f32, duration as f32);
        s.store_sound(slot, buf);
    });
    BCPL_AUDIO_OK
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_DistantExplode(slot: i64, distance: f64, duration: f64) -> i64 {
    with_state(|s| {
        let buf = s.engine.distant_explosion(distance as f32, duration as f32);
        s.store_sound(slot, buf);
    });
    BCPL_AUDIO_OK
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_MetalExplode(slot: i64, shrapnel: f64, duration: f64) -> i64 {
    with_state(|s| {
        let buf = s.engine.metal_explosion(shrapnel as f32, duration as f32);
        s.store_sound(slot, buf);
    });
    BCPL_AUDIO_OK
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_Zap(slot: i64, frequency: f64, duration: f64) -> i64 {
    with_state(|s| {
        let buf = s.engine.zap(frequency as f32, duration as f32);
        s.store_sound(slot, buf);
    });
    BCPL_AUDIO_OK
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_Shoot(slot: i64, power: f64, duration: f64) -> i64 {
    with_state(|s| {
        let buf = s.engine.shoot(power as f32, duration as f32);
        s.store_sound(slot, buf);
    });
    BCPL_AUDIO_OK
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_Powerup(slot: i64, intensity: f64, duration: f64) -> i64 {
    with_state(|s| {
        let buf = s.engine.powerup(intensity as f32, duration as f32);
        s.store_sound(slot, buf);
    });
    BCPL_AUDIO_OK
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_Hurt(slot: i64, severity: f64, duration: f64) -> i64 {
    with_state(|s| {
        let buf = s.engine.hurt(severity as f32, duration as f32);
        s.store_sound(slot, buf);
    });
    BCPL_AUDIO_OK
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_Click(slot: i64, sharpness: f64, duration: f64) -> i64 {
    with_state(|s| {
        let buf = s.engine.click(sharpness as f32, duration as f32);
        s.store_sound(slot, buf);
    });
    BCPL_AUDIO_OK
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_Bang(slot: i64, intensity: f64, duration: f64) -> i64 {
    with_state(|s| {
        let buf = s.engine.bang(intensity as f32, duration as f32);
        s.store_sound(slot, buf);
    });
    BCPL_AUDIO_OK
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_Blip(slot: i64, pitch: f64, duration: f64) -> i64 {
    with_state(|s| {
        let buf = s.engine.blip(pitch as f32, duration as f32);
        s.store_sound(slot, buf);
    });
    BCPL_AUDIO_OK
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_Pickup(slot: i64, brightness: f64, duration: f64) -> i64 {
    with_state(|s| {
        let buf = s.engine.pickup(brightness as f32, duration as f32);
        s.store_sound(slot, buf);
    });
    BCPL_AUDIO_OK
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_SweepUp(
    slot: i64,
    start_freq: f64,
    end_freq: f64,
    duration: f64,
) -> i64 {
    with_state(|s| {
        let buf = s.engine.sweep_up(start_freq as f32, end_freq as f32, duration as f32);
        s.store_sound(slot, buf);
    });
    BCPL_AUDIO_OK
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_SweepDown(
    slot: i64,
    start_freq: f64,
    end_freq: f64,
    duration: f64,
) -> i64 {
    with_state(|s| {
        let buf = s.engine.sweep_down(start_freq as f32, end_freq as f32, duration as f32);
        s.store_sound(slot, buf);
    });
    BCPL_AUDIO_OK
}

/// `Sound_RandomBeep(slot, seed, duration)` — `seed` is an integer.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_RandomBeep(slot: i64, seed: i64, duration: f64) -> i64 {
    with_state(|s| {
        let buf = s.engine.random_beep(seed as u32, duration as f32);
        s.store_sound(slot, buf);
    });
    BCPL_AUDIO_OK
}

// ─── Sound custom synthesis ───────────────────────────────────────

/// `Sound_Tone(slot, freq, duration, waveform)`.
///
/// `waveform` is the integer code from `newaudio_core::Waveform`:
/// 0=Sine, 1=Square, 2=Sawtooth, 3=Triangle, 4=Noise, 5=Pulse.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_Tone(slot: i64, freq: f64, duration: f64, waveform: i64) -> i64 {
    with_state(|s| {
        let wf = Waveform::from_code(waveform as i32);
        let buf = s.engine.tone(freq as f32, duration as f32, wf);
        s.store_sound(slot, buf);
    });
    BCPL_AUDIO_OK
}

/// `Sound_Note(slot, midi, dur, wf, attack, decay, sustain, release)`.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_Note(
    slot: i64,
    midi: i64,
    duration: f64,
    waveform: i64,
    attack: f64,
    decay: f64,
    sustain: f64,
    release: f64,
) -> i64 {
    with_state(|s| {
        let wf = Waveform::from_code(waveform as i32);
        let env = Adsr {
            attack: attack as f32,
            decay: decay as f32,
            sustain: sustain as f32,
            release: release as f32,
        };
        let buf = s.engine.midi_note(midi as i32, duration as f32, wf, env);
        s.store_sound(slot, buf);
    });
    BCPL_AUDIO_OK
}

/// `Sound_Noise(slot, noiseType, duration)`. `noiseType`: 0=white,
/// 1=pink, 2=brown.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_Noise(slot: i64, noise_type: i64, duration: f64) -> i64 {
    with_state(|s| {
        let kind = NoiseType::from_code(noise_type as i32);
        let buf = s.engine.noise(kind, duration as f32);
        s.store_sound(slot, buf);
    });
    BCPL_AUDIO_OK
}

/// `Sound_FM(slot, carrier, modulator, modIndex, dur)` — simple sine
/// FM synthesis. `mod_index` is in radians.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_FM(
    slot: i64,
    carrier_hz: f64,
    modulator_hz: f64,
    mod_index: f64,
    duration: f64,
) -> i64 {
    with_state(|s| {
        let buf = s.engine.fm(
            carrier_hz as f32,
            modulator_hz as f32,
            mod_index as f32,
            duration as f32,
        );
        s.store_sound(slot, buf);
    });
    BCPL_AUDIO_OK
}

// ─── Sound effect chains ──────────────────────────────────────────
//
// Each of these renders a tone and applies a built-in effect, then
// stores the result in the slot bank. They're not real-time effects
// — they bake the effect into the buffer at registration time. Live
// per-voice effects belong to a later phase.

/// `Sound_Reverb(slot, freq, dur, wf, roomSize, damping, wet)`.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_Reverb(
    slot: i64,
    frequency: f64,
    duration: f64,
    waveform: i64,
    room_size: f64,
    damping: f64,
    wet: f64,
) -> i64 {
    with_state(|s| {
        let wf = Waveform::from_code(waveform as i32);
        let buf = s.engine.reverb_tone(
            frequency as f32,
            duration as f32,
            wf,
            room_size as f32,
            damping as f32,
            wet as f32,
        );
        s.store_sound(slot, buf);
    });
    BCPL_AUDIO_OK
}

/// `Sound_Delay(slot, freq, dur, wf, delayTime, feedback, mix)`.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_Delay(
    slot: i64,
    frequency: f64,
    duration: f64,
    waveform: i64,
    delay_time: f64,
    feedback: f64,
    mix: f64,
) -> i64 {
    with_state(|s| {
        let wf = Waveform::from_code(waveform as i32);
        let buf = s.engine.delay_tone(
            frequency as f32,
            duration as f32,
            wf,
            delay_time as f32,
            feedback as f32,
            mix as f32,
        );
        s.store_sound(slot, buf);
    });
    BCPL_AUDIO_OK
}

/// `Sound_Distort(slot, freq, dur, wf, drive, tone, level)`.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_Distort(
    slot: i64,
    frequency: f64,
    duration: f64,
    waveform: i64,
    drive: f64,
    tone: f64,
    level: f64,
) -> i64 {
    with_state(|s| {
        let wf = Waveform::from_code(waveform as i32);
        let buf = s.engine.distortion_tone(
            frequency as f32,
            duration as f32,
            wf,
            drive as f32,
            tone as f32,
            level as f32,
        );
        s.store_sound(slot, buf);
    });
    BCPL_AUDIO_OK
}

/// `Sound_FilterTone(slot, freq, dur, wf, filterType, cutoff, resonance)`.
///
/// `filter_type`: 0=None, 1=LowPass, 2=HighPass, 3=BandPass (per
/// `newaudio_core::FilterType`).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_FilterTone(
    slot: i64,
    frequency: f64,
    duration: f64,
    waveform: i64,
    filter_type: i64,
    cutoff: f64,
    resonance: f64,
) -> i64 {
    with_state(|s| {
        let wf = Waveform::from_code(waveform as i32);
        let ft = FilterType::from_code(filter_type as i32);
        let buf = s.engine.filtered_tone(
            frequency as f32,
            duration as f32,
            wf,
            ft,
            cutoff as f32,
            resonance as f32,
        );
        s.store_sound(slot, buf);
    });
    BCPL_AUDIO_OK
}

/// `Sound_FilterNote(slot, midi, dur, wf, attack, decay, sustain,
/// release, filterType, cutoff, resonance)`.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C-unwind" fn Sound_FilterNote(
    slot: i64,
    midi: i64,
    duration: f64,
    waveform: i64,
    attack: f64,
    decay: f64,
    sustain: f64,
    release: f64,
    filter_type: i64,
    cutoff: f64,
    resonance: f64,
) -> i64 {
    with_state(|s| {
        let wf = Waveform::from_code(waveform as i32);
        let ft = FilterType::from_code(filter_type as i32);
        let env = Adsr {
            attack: attack as f32,
            decay: decay as f32,
            sustain: sustain as f32,
            release: release as f32,
        };
        let buf = s.engine.filtered_note(
            midi as i32,
            duration as f32,
            wf,
            env,
            ft,
            cutoff as f32,
            resonance as f32,
        );
        s.store_sound(slot, buf);
    });
    BCPL_AUDIO_OK
}

// ─── Sound playback ───────────────────────────────────────────────

/// `Sound_Play(slot, volume, pan)`. Returns `BCPL_AUDIO_OK` on
/// success, `BCPL_AUDIO_ERR_UNKNOWN_SLOT` if the slot was never
/// populated, or `BCPL_AUDIO_ERR_NO_DEVICE` if there's no working
/// audio device. `volume` is 0.0..=1.0; `pan` is -1.0 (left) to
/// 1.0 (right), 0.0 = centre.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_Play(slot: i64, volume: f64, pan: f64) -> i64 {
    with_state(|s| {
        if !s.sound_slots.contains_key(&slot) {
            return BCPL_AUDIO_ERR_UNKNOWN_SLOT;
        }
        #[cfg(windows)]
        {
            let na_id = s.sound_slots[&slot].na_id;
            let Some(m) = s.mixer() else {
                return BCPL_AUDIO_ERR_NO_DEVICE;
            };
            m.pcm().play(na_id, volume as f32, pan as f32);
        }
        #[cfg(not(windows))]
        let _ = (volume, pan);
        BCPL_AUDIO_OK
    })
}

/// `Sound_StopAll` — silence every active voice. The slot bank is
/// untouched; replay with `Sound_Play` later.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_StopAll() -> i64 {
    with_state(|s| {
        #[cfg(windows)]
        if let Some(m) = s.mixer.as_ref() {
            m.pcm().stop_all();
        }
        #[cfg(not(windows))]
        let _ = s;
    });
    BCPL_AUDIO_OK
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_Free(slot: i64) -> i64 {
    with_state(|s| {
        if s.free_sound(slot) {
            BCPL_AUDIO_OK
        } else {
            BCPL_AUDIO_ERR_UNKNOWN_SLOT
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_FreeAll() -> i64 {
    with_state(|s| s.free_all_sounds());
    BCPL_AUDIO_OK
}

/// `Sound_SetVolume(level)` — sets the SFX bus volume. Range `[0, 1]`.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_SetVolume(volume: f64) -> i64 {
    with_state(|s| {
        let v = (volume as f32).clamp(0.0, 1.0);
        s.sfx_volume = v;
        #[cfg(windows)]
        if let Some(m) = s.mixer.as_ref() {
            m.set_sfx_bus_volume(v);
        }
    });
    BCPL_AUDIO_OK
}

/// `Sound_GetVolume()` — reads back the SFX bus volume.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_GetVolume() -> f64 {
    with_state(|s| s.sfx_volume) as f64
}

/// `Sound_Count()` — number of populated sound slots.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_Count() -> i64 {
    with_state(|s| s.sound_slots.len() as i64)
}

/// `Sound_Playing(slot)` — `1` if at least one voice is currently
/// playing that slot's sound, `0` otherwise (also `0` for unknown
/// slots and on non-Windows targets).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_Playing(slot: i64) -> i64 {
    with_state(|s| {
        #[cfg(windows)]
        {
            let Some(entry) = s.sound_slots.get(&slot) else {
                return 0;
            };
            let na_id = entry.na_id;
            match s.mixer.as_ref() {
                Some(m) => i64::from(m.pcm().is_playing(na_id)),
                None => 0,
            }
        }
        #[cfg(not(windows))]
        {
            let _ = slot;
            let _ = s;
            0
        }
    })
}

/// `Sound_Duration(slot)` — seconds; 0.0 if the slot is empty.
/// Useful for game logic that wants to wait out a sound.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn Sound_Duration(slot: i64) -> f64 {
    with_state(|s| {
        s.sound_slots
            .get(&slot)
            .map(|x| x.duration as f64)
            .unwrap_or(0.0)
    })
}

// ─── Music ────────────────────────────────────────────────────────

/// `Music_Load(slot, abc_string)` — parse ABC notation, register the
/// MIDI asset, and bind it to `slot`. `abc_ptr` is a NUL-terminated
/// UTF-8 byte sequence (BCPL string literal). Returns `BCPL_AUDIO_OK`
/// on a clean parse, or `BCPL_AUDIO_ERR_PARSE` if the parser
/// rejected the input.
///
/// # Safety
/// `abc_ptr` must be null or point to a NUL-terminated byte sequence
/// for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn Music_Load(slot: i64, abc_ptr: *const u8) -> i64 {
    if abc_ptr.is_null() {
        return BCPL_AUDIO_ERR_PARSE;
    }
    let abc = match unsafe { CStr::from_ptr(abc_ptr as *const i8) }.to_str() {
        Ok(s) => s,
        Err(_) => return BCPL_AUDIO_ERR_PARSE,
    };
    let tune = match AbcParser::new().parse(abc) {
        Ok(t) => t,
        Err(_) => return BCPL_AUDIO_ERR_PARSE,
    };
    let title = tune.title.clone();
    let tempo_bpm = tune.default_tempo.bpm as f32;
    with_state(|s| {
        s.free_music(slot);
        #[cfg(windows)]
        let na_id = match s.mixer() {
            Some(m) => m.midi().load(&tune),
            None => 0,
        };
        #[cfg(not(windows))]
        let _ = &tune;
        s.music_slots.insert(
            slot,
            MusicSlot {
                title,
                tempo_bpm,
                #[cfg(windows)]
                na_id,
            },
        );
    });
    BCPL_AUDIO_OK
}

/// `Music_Play(slot, volume)`. Returns `BCPL_AUDIO_OK` if playback
/// started, `BCPL_AUDIO_ERR_UNKNOWN_SLOT` if the slot is empty, or
/// `BCPL_AUDIO_ERR_NO_DEVICE` if there's no MIDI device.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn Music_Play(slot: i64, volume: f64) -> i64 {
    with_state(|s| {
        if !s.music_slots.contains_key(&slot) {
            return BCPL_AUDIO_ERR_UNKNOWN_SLOT;
        }
        #[cfg(windows)]
        {
            let na_id = s.music_slots[&slot].na_id;
            let Some(m) = s.mixer() else {
                return BCPL_AUDIO_ERR_NO_DEVICE;
            };
            if !m.midi().play(na_id, volume as f32) {
                return BCPL_AUDIO_ERR_NO_DEVICE;
            }
        }
        #[cfg(not(windows))]
        let _ = volume;
        BCPL_AUDIO_OK
    })
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn Music_StopAll() -> i64 {
    with_state(|s| {
        #[cfg(windows)]
        if let Some(m) = s.mixer.as_ref() {
            m.midi().stop_all();
        }
        #[cfg(not(windows))]
        let _ = s;
    });
    BCPL_AUDIO_OK
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn Music_PauseAll() -> i64 {
    with_state(|s| {
        #[cfg(windows)]
        if let Some(m) = s.mixer.as_ref() {
            m.midi().pause();
        }
        #[cfg(not(windows))]
        let _ = s;
    });
    BCPL_AUDIO_OK
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn Music_ResumeAll() -> i64 {
    with_state(|s| {
        #[cfg(windows)]
        if let Some(m) = s.mixer.as_ref() {
            m.midi().resume();
        }
        #[cfg(not(windows))]
        let _ = s;
    });
    BCPL_AUDIO_OK
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn Music_Free(slot: i64) -> i64 {
    with_state(|s| {
        if s.free_music(slot) {
            BCPL_AUDIO_OK
        } else {
            BCPL_AUDIO_ERR_UNKNOWN_SLOT
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn Music_FreeAll() -> i64 {
    with_state(|s| s.free_all_music());
    BCPL_AUDIO_OK
}

/// `Music_SetVolume(level)` — sets the music bus volume. Range `[0, 1]`.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn Music_SetVolume(volume: f64) -> i64 {
    with_state(|s| {
        let v = (volume as f32).clamp(0.0, 1.0);
        s.music_volume = v;
        #[cfg(windows)]
        if let Some(m) = s.mixer.as_ref() {
            m.set_music_bus_volume(v);
        }
    });
    BCPL_AUDIO_OK
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn Music_GetVolume() -> f64 {
    with_state(|s| s.music_volume) as f64
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn Music_Count() -> i64 {
    with_state(|s| s.music_slots.len() as i64)
}

/// `Music_State()` — global playback state: `MUSIC_STATE_STOPPED`,
/// `MUSIC_STATE_PLAYING`, or `MUSIC_STATE_PAUSED`. Always
/// `MUSIC_STATE_STOPPED` on non-Windows.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn Music_State() -> i64 {
    with_state(|s| {
        #[cfg(windows)]
        {
            match s.mixer.as_ref().map(|m| m.midi().state()) {
                Some(PlaybackState::Playing) => MUSIC_STATE_PLAYING,
                Some(PlaybackState::Paused) => MUSIC_STATE_PAUSED,
                _ => MUSIC_STATE_STOPPED,
            }
        }
        #[cfg(not(windows))]
        {
            let _ = s;
            MUSIC_STATE_STOPPED
        }
    })
}

/// `Music_Playing(slot)` — `1` if the slot's tune currently has an
/// active playback (playing or paused), `0` otherwise.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn Music_Playing(slot: i64) -> i64 {
    with_state(|s| {
        #[cfg(windows)]
        {
            let Some(entry) = s.music_slots.get(&slot) else {
                return 0;
            };
            let na_id = entry.na_id;
            match s.mixer.as_ref() {
                Some(m) => i64::from(m.midi().is_asset_playing(na_id)),
                None => 0,
            }
        }
        #[cfg(not(windows))]
        {
            let _ = slot;
            let _ = s;
            0
        }
    })
}

/// `Music_Tempo(slot)` — tempo in BPM; 0.0 for unknown slots.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn Music_Tempo(slot: i64) -> f64 {
    with_state(|s| {
        s.music_slots
            .get(&slot)
            .map(|x| x.tempo_bpm as f64)
            .unwrap_or(0.0)
    })
}

#[cfg(test)]
pub(crate) fn music_title_snapshot(slot: i64) -> Option<String> {
    with_state(|s| s.music_slots.get(&slot).map(|x| x.title.clone()))
}

// ─── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// The runtime is process-global. Running tests in parallel would
    /// let `Sound_Coin(1, ...)` and `Sound_Zap(1, ...)` race. Serialise
    /// the suite under a single lock — and reset state at the top of
    /// each test.
    static TEST_LOCK: StdMutex<()> = StdMutex::new(());

    fn reset() {
        Sound_FreeAll();
        Music_FreeAll();
        Sound_SetVolume(1.0);
        Music_SetVolume(1.0);
    }

    #[test]
    fn sound_preset_registers_a_slot() {
        let _g = TEST_LOCK.lock().unwrap();
        reset();

        assert_eq!(Sound_Count(), 0);
        Sound_Coin(1, 1.0, 0.1);
        assert_eq!(Sound_Count(), 1);
        let d = Sound_Duration(1);
        assert!(d > 0.05, "duration {d} should exceed 0.05");
        assert!(d <= 0.11, "duration {d} should be at most 0.11");
    }

    #[test]
    fn rebinding_a_slot_replaces_the_old_buffer() {
        let _g = TEST_LOCK.lock().unwrap();
        reset();

        Sound_Coin(7, 1.0, 0.1);
        let d1 = Sound_Duration(7);
        Sound_Explode(7, 1.0, 0.5);
        let d2 = Sound_Duration(7);
        assert_eq!(Sound_Count(), 1);
        assert!(d2 > d1, "expected longer explode buffer; got {d1} → {d2}");
    }

    #[test]
    fn free_removes_the_slot() {
        let _g = TEST_LOCK.lock().unwrap();
        reset();

        Sound_Jump(3, 1.0, 0.2);
        Sound_Zap(4, 440.0, 0.2);
        assert_eq!(Sound_Count(), 2);
        assert_eq!(Sound_Free(3), BCPL_AUDIO_OK);
        assert_eq!(Sound_Count(), 1);
        assert_eq!(Sound_Free(3), BCPL_AUDIO_ERR_UNKNOWN_SLOT);
        Sound_FreeAll();
        assert_eq!(Sound_Count(), 0);
    }

    #[test]
    fn volumes_round_trip_and_clamp() {
        let _g = TEST_LOCK.lock().unwrap();
        reset();

        Sound_SetVolume(0.5);
        assert!((Sound_GetVolume() - 0.5).abs() < 1e-6);
        Music_SetVolume(0.25);
        assert!((Music_GetVolume() - 0.25).abs() < 1e-6);

        Sound_SetVolume(2.0);
        assert!((Sound_GetVolume() - 1.0).abs() < 1e-6);
        Sound_SetVolume(-1.0);
        assert!(Sound_GetVolume().abs() < 1e-6);
    }

    #[test]
    fn play_unknown_slot_reports_error() {
        let _g = TEST_LOCK.lock().unwrap();
        reset();

        assert_eq!(Sound_Play(99, 1.0, 0.0), BCPL_AUDIO_ERR_UNKNOWN_SLOT);
        assert_eq!(Sound_Playing(99), 0);
    }

    #[test]
    fn custom_synth_shims_populate_slots() {
        let _g = TEST_LOCK.lock().unwrap();
        reset();

        Sound_Tone(10, 440.0, 0.05, 1); // Square
        Sound_Noise(11, 1, 0.05); // Pink
        Sound_Note(12, 60, 0.05, 0, 0.01, 0.05, 0.7, 0.1);
        assert_eq!(Sound_Count(), 3);
        assert!(Sound_Duration(10) > 0.0);
        assert!(Sound_Duration(11) > 0.0);
        assert!(Sound_Duration(12) > 0.0);
    }

    #[test]
    fn music_load_accepts_a_valid_tune() {
        let _g = TEST_LOCK.lock().unwrap();
        reset();

        let abc = c"X:1\nT:Demo\nM:4/4\nL:1/4\nQ:120\nK:C\nCDEF|GAGE|";
        let rc = unsafe { Music_Load(1, abc.as_ptr() as *const u8) };
        assert_eq!(rc, BCPL_AUDIO_OK);
        assert_eq!(Music_Count(), 1);
        let title = music_title_snapshot(1).unwrap_or_default();
        assert_eq!(title, "Demo");
        assert!(Music_Tempo(1) > 0.0);
    }

    #[test]
    fn music_load_rejects_null() {
        let _g = TEST_LOCK.lock().unwrap();
        reset();

        let rc = unsafe { Music_Load(2, std::ptr::null()) };
        assert_eq!(rc, BCPL_AUDIO_ERR_PARSE);
        assert_eq!(Music_Count(), 0);
    }

    #[test]
    fn fm_and_effect_chains_populate_slots() {
        let _g = TEST_LOCK.lock().unwrap();
        reset();

        Sound_FM(20, 440.0, 110.0, 4.0, 0.05);
        Sound_Reverb(21, 440.0, 0.05, 0, 0.5, 0.5, 0.3);
        Sound_Delay(22, 440.0, 0.05, 0, 0.05, 0.4, 0.3);
        Sound_Distort(23, 220.0, 0.05, 1, 0.7, 0.5, 0.9);
        Sound_FilterTone(24, 880.0, 0.05, 2, 1, 1500.0, 0.0);
        Sound_FilterNote(25, 60, 0.05, 0, 0.01, 0.05, 0.7, 0.1, 1, 1500.0, 0.0);
        assert_eq!(Sound_Count(), 6);
        for slot in 20..=25 {
            assert!(
                Sound_Duration(slot) > 0.0,
                "slot {slot} should have a buffer"
            );
        }
    }

    #[test]
    fn music_free_removes_the_slot() {
        let _g = TEST_LOCK.lock().unwrap();
        reset();

        let abc = c"X:1\nT:F\nK:C\nC|";
        assert_eq!(
            unsafe { Music_Load(5, abc.as_ptr() as *const u8) },
            BCPL_AUDIO_OK
        );
        assert_eq!(Music_Count(), 1);
        assert_eq!(Music_Free(5), BCPL_AUDIO_OK);
        assert_eq!(Music_Count(), 0);
        assert_eq!(Music_Free(5), BCPL_AUDIO_ERR_UNKNOWN_SLOT);
    }
}
