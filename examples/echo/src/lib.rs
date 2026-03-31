//! Example APO: stereo echo/delay with feedback.
//!
//! Adds a 250ms delayed copy of the audio mixed back into the output,
//! with configurable feedback (repeated echoes) and wet/dry mix.
//!
//! Demonstrates:
//! - Pre-allocating state in `initialize()` (no alloc in `process()`)
//! - Adapting to sample rate and channel count in `lock()`
//! - Ring buffer technique for real-time delay
//! - Reporting latency to the audio engine

use windows_apo::{ApoProcessor, ApoConfig, ProcessContext, apo_flags};

// {20000002-0002-4000-8000-000000000002}
const CLSID: [u8; 16] = [
    0x02, 0x00, 0x00, 0x20,
    0x02, 0x00,
    0x00, 0x40,
    0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02,
];

// {20000002-0002-4000-8000-E00000000002}
const EFFECT_ECHO: [u8; 16] = [
    0x02, 0x00, 0x00, 0x20,
    0x02, 0x00,
    0x00, 0x40,
    0x80, 0x00, 0xE0, 0x00, 0x00, 0x00, 0x00, 0x02,
];

static EFFECTS: [[u8; 16]; 1] = [EFFECT_ECHO];

static CONFIG: ApoConfig = ApoConfig {
    clsid: CLSID,
    name: &windows_apo::wide!("Echo Example APO"),
    copyright: &windows_apo::wide!("Public Domain"),
    apo_flags: apo_flags::INPLACE
        | apo_flags::FRAMESPERSECOND_MUST_MATCH
        | apo_flags::BITSPERSAMPLE_MUST_MATCH
        | apo_flags::SAMPLESPERFRAME_MUST_MATCH,
    effects: &EFFECTS,
};

// ---------------------------------------------------------------------------

/// Maximum delay we support (1 second at 192 kHz, 8 channels).
/// Pre-allocated once to avoid runtime allocation.
const MAX_DELAY_SAMPLES: usize = 192_000;
const MAX_CHANNELS: usize = 8;

struct EchoProcessor {
    // Parameters
    delay_ms: f32,   // echo delay in milliseconds
    feedback: f32,   // 0.0 = single echo, 0.5 = repeated echoes, <1.0 to be stable
    wet: f32,        // wet/dry mix (0.0 = dry only, 1.0 = echo only)

    // State -- pre-allocated ring buffer per channel
    buffer: Vec<f32>,           // MAX_DELAY_SAMPLES * MAX_CHANNELS
    write_pos: usize,           // current write position in samples (not frames)
    delay_samples: usize,       // delay in samples (computed from delay_ms + sample_rate)
    channels: usize,
    sample_rate: f32,
}

impl EchoProcessor {
    fn new() -> Self {
        Self {
            delay_ms: 250.0,
            feedback: 0.35,
            wet: 0.4,
            buffer: Vec::new(), // allocated in initialize()
            write_pos: 0,
            delay_samples: 0,
            channels: 0,
            sample_rate: 48000.0,
        }
    }
}

impl ApoProcessor for EchoProcessor {
    fn initialize(&mut self, _sample_rate: f32) -> Result<(), String> {
        // Pre-allocate the delay buffer. This is the only allocation we do.
        // process() will never allocate.
        self.buffer = vec![0.0f32; MAX_DELAY_SAMPLES * MAX_CHANNELS];
        Ok(())
    }

    fn lock(&mut self, ctx: &ProcessContext) {
        self.sample_rate = ctx.sample_rate;
        self.channels = ctx.input_channels as usize;

        // Compute delay in samples from milliseconds
        self.delay_samples = ((self.delay_ms / 1000.0) * ctx.sample_rate) as usize;
        if self.delay_samples > MAX_DELAY_SAMPLES {
            self.delay_samples = MAX_DELAY_SAMPLES;
        }
        if self.delay_samples == 0 {
            self.delay_samples = 1;
        }

        // Reset state
        self.write_pos = 0;
        if !self.buffer.is_empty() {
            self.buffer.iter_mut().for_each(|s| *s = 0.0);
        }
    }

    fn unlock(&mut self) {}

    fn process(&mut self, buffer: &mut [f32], frames: u32, channels: u32, is_silent: bool) {
        if is_silent || self.buffer.is_empty() {
            return;
        }

        let ch = channels as usize;
        let delay = self.delay_samples;

        for frame in 0..frames as usize {
            for c in 0..ch {
                let buf_idx = frame * ch + c;
                let dry = buffer[buf_idx];

                // Read from delay buffer (delay_samples behind write_pos)
                let read_pos = if self.write_pos >= delay {
                    self.write_pos - delay
                } else {
                    MAX_DELAY_SAMPLES - (delay - self.write_pos)
                };
                let ring_idx = read_pos * MAX_CHANNELS + c;
                let delayed = if ring_idx < self.buffer.len() {
                    self.buffer[ring_idx]
                } else {
                    0.0
                };

                // Write to delay buffer: input + feedback * delayed
                let ring_write = self.write_pos * MAX_CHANNELS + c;
                if ring_write < self.buffer.len() {
                    self.buffer[ring_write] = dry + self.feedback * delayed;
                }

                // Mix: dry + wet * delayed
                buffer[buf_idx] = dry + self.wet * delayed;
            }

            self.write_pos += 1;
            if self.write_pos >= MAX_DELAY_SAMPLES {
                self.write_pos = 0;
            }
        }
    }

    fn latency(&self) -> i64 {
        // Report the delay to the audio engine in HNSTIME (100ns units).
        // This lets the engine compensate for the delay in video sync.
        let delay_seconds = self.delay_ms / 1000.0;
        (delay_seconds * 10_000_000.0) as i64
    }
}

windows_apo::apo_entry!(CONFIG, || EchoProcessor::new());
