//! Minimal APO example: applies a fixed gain (volume reduction) to all audio.
//!
//! This is the simplest possible APO -- it multiplies every sample by a constant.
//! Use it as a starting point for your own APO.

use windows_apo::{ApoProcessor, ApoConfig, ProcessContext, apo_flags};

// CLSID for this APO -- generate your own with `uuidgen`.
// {10000001-0001-4000-8000-000000000001}
const CLSID: [u8; 16] = [
    0x01, 0x00, 0x00, 0x10,  // Data1: 0x10000001 LE
    0x01, 0x00,              // Data2: 0x0001 LE
    0x00, 0x40,              // Data3: 0x4000 LE
    0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
];

// Effect GUID shown in Windows Settings
// {10000001-0001-4000-8000-E00000000001}
const EFFECT_GAIN: [u8; 16] = [
    0x01, 0x00, 0x00, 0x10,
    0x01, 0x00,
    0x00, 0x40,
    0x80, 0x00, 0xE0, 0x00, 0x00, 0x00, 0x00, 0x01,
];

static EFFECTS: [[u8; 16]; 1] = [EFFECT_GAIN];

static CONFIG: ApoConfig = ApoConfig {
    clsid: CLSID,
    name: &windows_apo::wide!("Gain Example APO"),
    copyright: &windows_apo::wide!("Public Domain"),
    apo_flags: apo_flags::INPLACE
        | apo_flags::FRAMESPERSECOND_MUST_MATCH
        | apo_flags::BITSPERSAMPLE_MUST_MATCH
        | apo_flags::SAMPLESPERFRAME_MUST_MATCH,
    effects: &EFFECTS,
};

// ---------------------------------------------------------------------------

struct GainProcessor {
    /// Linear gain factor. 0.5 = -6 dB (half volume).
    gain: f32,
}

impl GainProcessor {
    fn new() -> Self {
        Self { gain: 0.5 }
    }
}

impl ApoProcessor for GainProcessor {
    fn initialize(&mut self, _sample_rate: f32) -> Result<(), String> {
        // Nothing to load -- gain is hardcoded.
        // A real APO might read a config file here.
        Ok(())
    }

    fn lock(&mut self, _ctx: &ProcessContext) {
        // Format is locked -- we could adapt to channel count here.
    }

    fn unlock(&mut self) {}

    fn process(&mut self, buffer: &mut [f32], _frames: u32, _channels: u32, is_silent: bool) {
        if is_silent {
            return;
        }
        for sample in buffer.iter_mut() {
            *sample *= self.gain;
        }
    }
}

windows_apo::apo_entry!(CONFIG, || GainProcessor::new());
