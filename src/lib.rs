//! Generic Windows Audio Processing Object (APO) framework for Rust.
//!
//! This crate provides a thin C++ skeleton that handles COM/APO boilerplate
//! (inheriting from `CBaseAudioProcessingObject`) and forwards all audio
//! processing to a Rust trait implementation via FFI callbacks.
//!
//! # Usage
//!
//! 1. Implement [`ApoProcessor`] for your audio processor struct.
//! 2. Define an [`ApoConfig`] with your CLSID, name, and effect GUIDs.
//! 3. Call [`apo_entry!`] in your `lib.rs` to generate the FFI bridge.
//! 4. In your `build.rs`, call [`sdk::build_apo`] to compile the C++ skeleton.
//!
//! ```rust,ignore
//! use windows_apo::{ApoProcessor, ApoConfig, ProcessContext};
//!
//! struct MyProcessor { /* ... */ }
//!
//! impl ApoProcessor for MyProcessor {
//!     fn initialize(&mut self, sample_rate: f32) -> Result<(), String> { Ok(()) }
//!     fn lock(&mut self, ctx: &ProcessContext) {}
//!     fn unlock(&mut self) {}
//!     fn process(&mut self, buffer: &mut [f32], frames: u32, channels: u32, is_silent: bool) {
//!         // Your real-time audio processing here
//!     }
//! }
//!
//! static CONFIG: ApoConfig = ApoConfig { /* ... */ };
//! windows_apo::apo_entry!(CONFIG, || MyProcessor::new());
//! ```

pub mod ffi_types;
pub mod sdk;

pub use ffi_types::{ApoCallbacks, ApoRegistration};

/// Context provided to [`ApoProcessor::lock`] with the negotiated audio format.
#[derive(Debug, Clone, Copy)]
pub struct ProcessContext {
    pub sample_rate: f32,
    pub input_channels: u32,
    pub output_channels: u32,
}

/// APO flags matching `APO_FLAG` values from the Windows SDK.
pub mod apo_flags {
    /// APO processes audio in-place (input buffer == output buffer).
    pub const INPLACE: u32 = 0x1;
    /// Input and output must have the same frames-per-second (sample rate).
    pub const FRAMESPERSECOND_MUST_MATCH: u32 = 0x2;
    /// Input and output must have the same bits-per-sample.
    pub const BITSPERSAMPLE_MUST_MATCH: u32 = 0x4;
    /// Input and output must have the same samples-per-frame (channel count).
    pub const SAMPLESPERFRAME_MUST_MATCH: u32 = 0x10;
}

/// Static configuration for an APO, used for COM/APO registration.
///
/// Provide this as a `static` and pass it to [`apo_entry!`].
pub struct ApoConfig {
    /// CLSID identifying this APO (16-byte raw GUID, same layout as Windows GUID).
    pub clsid: [u8; 16],
    /// Display name (null-terminated UTF-16 wide string).
    pub name: &'static [u16],
    /// Copyright string (null-terminated UTF-16 wide string).
    pub copyright: &'static [u16],
    /// `APO_FLAG` bitmask (see [`apo_flags`]).
    pub apo_flags: u32,
    /// Effect GUIDs exposed via `IAudioSystemEffects2/3`.
    pub effects: &'static [[u8; 16]],
}

/// Trait for implementing audio processing in Rust.
///
/// Implement this trait and pass a factory closure to [`apo_entry!`].
/// The C++ APO skeleton will call these methods at each lifecycle stage.
///
/// # Safety
///
/// [`process`](ApoProcessor::process) is called from the Windows MMCSS real-time
/// audio thread. It **must not** allocate, block, lock mutexes, or panic.
pub trait ApoProcessor: Send + 'static {
    /// Called after the APO is initialized by the audio engine.
    /// Not called in discovery-only mode (Windows querying effect lists).
    ///
    /// Use this to load resources (IR files, configs, etc.).
    fn initialize(&mut self, sample_rate: f32) -> Result<(), String>;

    /// Called when the audio engine locks the format for processing.
    fn lock(&mut self, ctx: &ProcessContext);

    /// Called when the audio engine unlocks (e.g., format change or shutdown).
    fn unlock(&mut self);

    /// Real-time audio processing.
    ///
    /// `buffer` is interleaved float32 samples, `frames * channels` elements.
    /// For in-place APOs, this is both the input and output buffer.
    /// `is_silent` indicates the engine sent `BUFFER_SILENT` (buffer is zeroed).
    fn process(&mut self, buffer: &mut [f32], frames: u32, channels: u32, is_silent: bool);

    /// Called when Windows changes an effect's enabled/disabled state.
    /// `effect_id` is the 16-byte GUID of the effect.
    fn on_effect_state_changed(&mut self, _effect_id: &[u8; 16], _enabled: bool) {}

    /// Return the processing latency in HNSTIME (100-nanosecond units).
    fn latency(&self) -> i64 {
        0
    }
}

/// Helper to create a null-terminated UTF-16 wide string at compile time.
///
/// Usage: `const NAME: &[u16] = &windows_apo::wide!("My APO Name");`
#[macro_export]
macro_rules! wide {
    ($s:expr) => {{
        const RESULT: &[u16] = {
            const INPUT: &str = $s;
            const LEN: usize = INPUT.len() + 1; // +1 for null terminator
            const fn encode(input: &str) -> [u16; LEN] {
                let bytes = input.as_bytes();
                let mut out = [0u16; LEN];
                let mut i = 0;
                while i < bytes.len() {
                    out[i] = bytes[i] as u16;
                    i += 1;
                }
                // out[LEN-1] is already 0 (null terminator)
                out
            }
            &{
                // Need this indirection for const context
                const LEN: usize = $s.len() + 1;
                const fn encode(input: &[u8]) -> [u16; LEN] {
                    let mut out = [0u16; LEN];
                    let mut i = 0;
                    while i < input.len() {
                        out[i] = input[i] as u16;
                        i += 1;
                    }
                    out
                }
                encode($s.as_bytes())
            }
        };
        RESULT
    }};
}

/// Generate the FFI bridge between the C++ APO skeleton and your [`ApoProcessor`] implementation.
///
/// This macro generates three `extern "C"` functions that the C++ skeleton calls:
/// - `apo_get_callbacks()` — returns the static callback vtable
/// - `apo_get_registration()` — returns static APO registration info
/// - `apo_create_processor()` — creates a new processor instance (called per APO instance)
///
/// # Arguments
/// - `$config` — a `static ApoConfig` with your APO's CLSID, name, flags, and effects
/// - `$factory` — a closure `|| -> impl ApoProcessor` that creates processor instances
///
/// # Example
/// ```rust,ignore
/// windows_apo::apo_entry!(MY_CONFIG, || MyProcessor::new());
/// ```
#[macro_export]
macro_rules! apo_entry {
    ($config:expr, $factory:expr) => {
        // ── FFI callback wrappers ──────────────────────────────────────

        unsafe extern "C" fn __apo_ffi_on_initialize(
            user_data: *mut ::std::ffi::c_void,
            sample_rate: f32,
        ) -> bool {
            if user_data.is_null() {
                return false;
            }
            let processor =
                unsafe { &mut *(user_data as *mut Box<dyn $crate::ApoProcessor>) };
            processor.initialize(sample_rate).is_ok()
        }

        unsafe extern "C" fn __apo_ffi_on_lock(
            user_data: *mut ::std::ffi::c_void,
            in_channels: u32,
            out_channels: u32,
            sample_rate: f32,
        ) -> bool {
            if user_data.is_null() {
                return false;
            }
            let processor =
                unsafe { &mut *(user_data as *mut Box<dyn $crate::ApoProcessor>) };
            processor.lock(&$crate::ProcessContext {
                sample_rate,
                input_channels: in_channels,
                output_channels: out_channels,
            });
            true
        }

        unsafe extern "C" fn __apo_ffi_on_unlock(user_data: *mut ::std::ffi::c_void) {
            if user_data.is_null() {
                return;
            }
            let processor =
                unsafe { &mut *(user_data as *mut Box<dyn $crate::ApoProcessor>) };
            processor.unlock();
        }

        unsafe extern "C" fn __apo_ffi_process(
            user_data: *mut ::std::ffi::c_void,
            buffer: *mut f32,
            n_frames: u32,
            channels: u32,
            is_silent: bool,
        ) {
            if user_data.is_null() || buffer.is_null() || n_frames == 0 {
                return;
            }
            let processor =
                unsafe { &mut *(user_data as *mut Box<dyn $crate::ApoProcessor>) };
            let len = (n_frames as usize) * (channels as usize);
            let buf = unsafe { ::std::slice::from_raw_parts_mut(buffer, len) };
            processor.process(buf, n_frames, channels, is_silent);
        }

        unsafe extern "C" fn __apo_ffi_destroy(user_data: *mut ::std::ffi::c_void) {
            if !user_data.is_null() {
                unsafe {
                    drop(Box::from_raw(
                        user_data as *mut Box<dyn $crate::ApoProcessor>,
                    ));
                }
            }
        }

        unsafe extern "C" fn __apo_ffi_on_effect_state(
            user_data: *mut ::std::ffi::c_void,
            effect_id: *const u8,
            state: i32,
        ) {
            if user_data.is_null() || effect_id.is_null() {
                return;
            }
            let processor =
                unsafe { &mut *(user_data as *mut Box<dyn $crate::ApoProcessor>) };
            let guid = unsafe { &*(effect_id as *const [u8; 16]) };
            processor.on_effect_state_changed(guid, state != 0);
        }

        unsafe extern "C" fn __apo_ffi_get_latency(
            user_data: *mut ::std::ffi::c_void,
        ) -> i64 {
            if user_data.is_null() {
                return 0;
            }
            let processor =
                unsafe { &*(user_data as *const Box<dyn $crate::ApoProcessor>) };
            processor.latency()
        }

        // ── Static callback vtable ─────────────────────────────────────

        #[no_mangle]
        pub extern "C" fn apo_get_callbacks() -> *const $crate::ffi_types::ApoCallbacks {
            static CALLBACKS: $crate::ffi_types::ApoCallbacks =
                $crate::ffi_types::ApoCallbacks {
                    on_initialize: Some(__apo_ffi_on_initialize),
                    on_lock: Some(__apo_ffi_on_lock),
                    on_unlock: Some(__apo_ffi_on_unlock),
                    process: Some(__apo_ffi_process),
                    destroy: Some(__apo_ffi_destroy),
                    on_effect_state: Some(__apo_ffi_on_effect_state),
                    get_latency: Some(__apo_ffi_get_latency),
                };
            &CALLBACKS as *const _
        }

        // ── Static registration info ───────────────────────────────────

        #[no_mangle]
        pub extern "C" fn apo_get_registration() -> *const $crate::ffi_types::ApoRegistration
        {
            static REGISTRATION: $crate::ffi_types::ApoRegistration =
                $crate::ffi_types::ApoRegistration {
                    clsid: $config.clsid,
                    name: $config.name.as_ptr(),
                    copyright: $config.copyright.as_ptr(),
                    apo_flags: $config.apo_flags,
                    effect_guids: $config.effects.as_ptr(),
                    num_effects: $config.effects.len() as u32,
                };
            &REGISTRATION as *const _
        }

        // ── Processor factory ──────────────────────────────────────────

        #[no_mangle]
        pub extern "C" fn apo_create_processor() -> *mut ::std::ffi::c_void {
            let processor: Box<dyn $crate::ApoProcessor> = Box::new(($factory)());
            let boxed: Box<Box<dyn $crate::ApoProcessor>> = Box::new(processor);
            Box::into_raw(boxed) as *mut ::std::ffi::c_void
        }
    };
}
