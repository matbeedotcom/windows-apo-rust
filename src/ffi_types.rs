//! FFI types shared between the Rust trait bridge and the C++ generic APO skeleton.
//!
//! These `#[repr(C)]` structs must match the layout in `GenericApo.h`.

use std::ffi::c_void;

/// Callback vtable passed from Rust to the C++ APO skeleton.
/// The C++ `GenericApo` class calls these function pointers at each lifecycle stage.
#[repr(C)]
pub struct ApoCallbacks {
    /// Called after base `Initialize` succeeds (skipped for discovery-only mode).
    /// `user_data`: opaque pointer returned by `apo_create_processor()`.
    /// `sample_rate`: audio engine sample rate.
    /// Returns `true` on success.
    pub on_initialize: Option<unsafe extern "C" fn(user_data: *mut c_void, sample_rate: f32) -> bool>,

    /// Called during `LockForProcess` after format is known.
    pub on_lock: Option<
        unsafe extern "C" fn(
            user_data: *mut c_void,
            in_channels: u32,
            out_channels: u32,
            sample_rate: f32,
        ) -> bool,
    >,

    /// Called during `UnlockForProcess`.
    pub on_unlock: Option<unsafe extern "C" fn(user_data: *mut c_void)>,

    /// Real-time audio processing callback. Called from MMCSS audio thread.
    /// MUST NOT allocate, block, or panic.
    pub process: Option<
        unsafe extern "C" fn(
            user_data: *mut c_void,
            buffer: *mut f32,
            n_frames: u32,
            channels: u32,
            is_silent: bool,
        ),
    >,

    /// Called to destroy the user_data when the APO instance is destroyed.
    pub destroy: Option<unsafe extern "C" fn(user_data: *mut c_void)>,

    /// Called when Windows changes an effect's enabled/disabled state.
    /// `effect_id`: pointer to a 16-byte GUID.
    /// `state`: 0 = off, 1 = on.
    pub on_effect_state:
        Option<unsafe extern "C" fn(user_data: *mut c_void, effect_id: *const u8, state: i32)>,

    /// Called to query latency in HNSTIME (100ns units). Returns 0 if null.
    pub get_latency: Option<unsafe extern "C" fn(user_data: *mut c_void) -> i64>,
}

// Safety: ApoCallbacks is a plain struct of function pointers, no interior mutability.
unsafe impl Send for ApoCallbacks {}
unsafe impl Sync for ApoCallbacks {}

/// Static APO registration info passed from Rust to C++ for `CRegAPOProperties`
/// and `DllRegisterServer`.
#[repr(C)]
pub struct ApoRegistration {
    /// CLSID as raw bytes (16 bytes, same layout as Windows GUID).
    pub clsid: [u8; 16],
    /// Wide-string name (null-terminated UTF-16). Pointer must be 'static.
    pub name: *const u16,
    /// Wide-string copyright. Pointer must be 'static.
    pub copyright: *const u16,
    /// APO_FLAG bitmask.
    pub apo_flags: u32,
    /// Array of effect GUIDs (each 16 bytes).
    pub effect_guids: *const [u8; 16],
    /// Number of effect GUIDs.
    pub num_effects: u32,
}

// Safety: ApoRegistration contains only pointers to static data.
unsafe impl Send for ApoRegistration {}
unsafe impl Sync for ApoRegistration {}
