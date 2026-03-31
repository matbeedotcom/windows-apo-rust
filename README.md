# windows-apo

Generic Windows Audio Processing Object (APO) framework for Rust.

Write your audio processing in Rust -- this crate handles the COM/C++ boilerplate that Windows requires for APOs. The C++ layer is a thin skeleton that inherits from `CBaseAudioProcessingObject` and forwards all lifecycle events and audio processing to your Rust `ApoProcessor` implementation via FFI callbacks.

## Architecture

```
  audiodg.exe (Windows audio engine)

  +-----------------------------------------------------+
  |  GenericApo.cpp (COM / CBaseAudioProcessingObject)   |
  |  |-- Initialize       -> on_initialize callback      |
  |  |-- LockForProcess   -> on_lock callback             |
  |  |-- APOProcess        -> process callback             |
  |  +-- UnlockForProcess -> on_unlock callback           |
  +------------------------+------------------------------+
                           | FFI
  +------------------------v------------------------------+
  |  Your Rust ApoProcessor implementation                |
  |  (convolution, EQ, compression, etc.)                 |
  +-------------------------------------------------------+
```

## Quick Start

### 1. Create a cdylib crate

```toml
# your-apo/Cargo.toml
[package]
name = "my-apo"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
windows-apo = { path = "../windows-apo" }

[build-dependencies]
windows-apo = { path = "../windows-apo" }
```

### 2. Write your build.rs

```rust
// your-apo/build.rs
fn main() {
    // Compiles the generic C++ skeleton and links APO libraries.
    // Pass any additional C++ files and include dirs your APO needs.
    windows_apo::sdk::build_apo(
        &[],   // extra C++ files (if any)
        &[],   // extra include directories
    );

    // Export standard DLL entry points
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let def = std::path::Path::new(&manifest).join("my_apo.def");
    println!("cargo:rustc-cdylib-link-arg=/DEF:{}", def.display());
}
```

### 3. Create a .def file

```def
; my_apo.def
LIBRARY my_apo
EXPORTS
    DllGetClassObject   PRIVATE
    DllCanUnloadNow     PRIVATE
    DllRegisterServer   PRIVATE
    DllUnregisterServer PRIVATE
```

### 4. Implement your processor

```rust
// your-apo/src/lib.rs
use windows_apo::{ApoProcessor, ApoConfig, ProcessContext, apo_flags};

// Generate a CLSID -- run `uuidgen` or use an online generator.
// Convert to little-endian byte layout (see GUID Encoding below).
const MY_CLSID: [u8; 16] = [
    0x01, 0x02, 0x03, 0x04, // Data1 LE
    0x05, 0x06,             // Data2 LE
    0x07, 0x08,             // Data3 LE
    0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10, // Data4
];

const MY_EFFECT: [u8; 16] = [
    0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18,
    0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F, 0x20,
];

static EFFECTS: [[u8; 16]; 1] = [MY_EFFECT];

static CONFIG: ApoConfig = ApoConfig {
    clsid: MY_CLSID,
    name: &windows_apo::wide!("My Audio Effect"),
    copyright: &windows_apo::wide!("Copyright (C) 2026"),
    apo_flags: apo_flags::INPLACE
        | apo_flags::FRAMESPERSECOND_MUST_MATCH
        | apo_flags::BITSPERSAMPLE_MUST_MATCH
        | apo_flags::SAMPLESPERFRAME_MUST_MATCH,
    effects: &EFFECTS,
};

struct MyProcessor {
    gain: f32,
}

impl MyProcessor {
    fn new() -> Self {
        Self { gain: 0.5 }
    }
}

impl ApoProcessor for MyProcessor {
    fn initialize(&mut self, sample_rate: f32) -> Result<(), String> {
        // Load resources, open files, etc.
        // NOT called in discovery-only mode.
        Ok(())
    }

    fn lock(&mut self, ctx: &ProcessContext) {
        // Audio format is now known.
        // ctx.sample_rate, ctx.input_channels, ctx.output_channels
    }

    fn unlock(&mut self) {
        // Audio engine is releasing the format lock.
    }

    fn process(&mut self, buffer: &mut [f32], frames: u32, channels: u32, is_silent: bool) {
        // Real-time audio processing -- runs on the MMCSS audio thread.
        // MUST NOT: allocate, lock mutexes, do I/O, or panic.
        if is_silent { return; }
        for sample in buffer.iter_mut() {
            *sample *= self.gain;
        }
    }
}

// Generate the FFI bridge (apo_get_callbacks, apo_get_registration, apo_create_processor)
windows_apo::apo_entry!(CONFIG, || MyProcessor::new());
```

### 5. Build and install

```bash
cargo build --release -p my-apo

# Register the APO (requires admin)
regsvr32 target/release/my_apo.dll
```

## API Reference

### `ApoProcessor` trait

| Method | When called | Real-time safe? | Required? |
|--------|-------------|-----------------|-----------|
| `initialize(sample_rate)` | After COM init, before audio flows | No | Yes |
| `lock(ctx)` | Audio engine locks format | No | Yes |
| `unlock()` | Audio engine releases format | No | Yes |
| `process(buffer, frames, channels, is_silent)` | Every audio buffer (~10ms) | **Yes** | Yes |
| `on_effect_state_changed(guid, enabled)` | Windows Settings toggles effect | No | No |
| `latency()` | Audio engine queries latency | Yes | No |

### `ApoConfig` struct

| Field | Type | Description |
|-------|------|-------------|
| `clsid` | `[u8; 16]` | COM class ID (GUID in LE byte layout) |
| `name` | `&'static [u16]` | Display name (UTF-16, use `wide!()` macro) |
| `copyright` | `&'static [u16]` | Copyright string (UTF-16) |
| `apo_flags` | `u32` | `APO_FLAG` bitmask (see `apo_flags` module) |
| `effects` | `&'static [[u8; 16]]` | Effect GUIDs for Windows Settings toggles |

### `apo_entry!` macro

```rust
windows_apo::apo_entry!(CONFIG_STATIC, || YourProcessor::new());
```

Generates three `extern "C"` functions the C++ skeleton calls:
- `apo_get_callbacks()` -- returns the callback vtable
- `apo_get_registration()` -- returns CLSID, name, flags, effects
- `apo_create_processor()` -- creates a new processor instance per APO instance

### `sdk::build_apo()` -- Build helper

```rust
// In your build.rs:
windows_apo::sdk::build_apo(
    &["cpp/MyDsp.cpp"],  // Additional C++ files
    &["cpp"],             // Additional include dirs
);
```

This function:
1. Auto-detects Visual Studio and Windows SDK paths (via `vswhere.exe` + filesystem scanning)
2. Compiles the generic C++ skeleton (`GenericApo.cpp`, `ClassFactory.cpp`, `DllMain.cpp`)
3. Compiles your additional C++ files
4. Emits `cargo:rustc-link-lib` for `audiobaseprocessingobject`, `ole32`, `oleaut32`, `uuid`, `avrt`

### `wide!` macro

```rust
const NAME: &[u16] = &windows_apo::wide!("My APO");
```

Creates a null-terminated UTF-16 array at compile time. Only supports ASCII input.

## GUID Encoding

Windows GUIDs have mixed endianness. For GUID `{AABBCCDD-EEFF-1122-3344-556677889900}`:

```
Data1 (u32 LE):  DD CC BB AA
Data2 (u16 LE):  FF EE
Data3 (u16 LE):  22 11
Data4 (bytes):   33 44 55 66 77 88 99 00
```

Example -- `{A1B2C3D4-E5F6-7890-ABCD-EF0123456789}`:
```rust
const CLSID: [u8; 16] = [
    0xD4, 0xC3, 0xB2, 0xA1,  // Data1: 0xa1b2c3d4 LE
    0xF6, 0xE5,              // Data2: 0xe5f6 LE
    0x90, 0x78,              // Data3: 0x7890 LE
    0xAB, 0xCD, 0xEF, 0x01, 0x23, 0x45, 0x67, 0x89,  // Data4
];
```

## APO Lifecycle

```
1. DllGetClassObject       -> ClassFactory creates GenericApo instance
2. GenericApo::Initialize  -> Parses APOInitSystemEffects v1/v2/v3
                           -> Calls ApoProcessor::initialize() (unless discovery-only)
3. GenericApo::LockForProcess -> Extracts format (channels, sample rate)
                              -> Calls ApoProcessor::lock()
4. GenericApo::APOProcess  -> Called every ~10ms on MMCSS audio thread
                           -> Calls ApoProcessor::process()
5. GenericApo::UnlockForProcess -> Calls ApoProcessor::unlock()
6. ~GenericApo             -> Calls ApoCallbacks::destroy (drops the processor)
```

## What the generic crate handles

- COM plumbing: `IUnknown`, `INonDelegatingUnknown`, aggregation support
- `CBaseAudioProcessingObject` inheritance (format validation, connection management)
- `IAudioSystemEffects3`: effect list, controllable effects, state change notifications
- `IAudioProcessingObjectNotifications`: endpoint property change registration
- `DllRegisterServer` / `DllUnregisterServer`: COM + APO registry entries
- Debug logging to `C:\ProgramData\HrtfApo\debug.log`
- SEH exception handling around `APOProcess`, `LockForProcess`, `IsInputFormatSupported`

## What you handle

- All audio DSP logic (in your `process()` method)
- Resource loading (IR files, configs, models)
- Any IPC with your GUI (shared memory, named pipes, etc.)
- Additional DLL exports (installation helpers, etc.)

## Requirements

- Windows 10/11 x64
- Visual Studio 2022 (Community, Professional, Enterprise, or BuildTools) with C++ workload
- Windows 10/11 SDK
- Rust stable (MSVC toolchain)

## Real-world example

See `hrtf-apo/` for a full consumer that implements HRTF binaural spatialization with:
- 8ch to 2ch convolution via `ApoProcessor::process()`
- Game audio enhancement (EQ, transient shaping, limiter)
- Real-time audio event classification
- Additional C++ DSP (footstep enhancer, feature extraction)
- Shared memory IPC with a GUI
- Audio capture system
- `InstallForEndpoint` / `RestoreSpeakerConfig` extra DLL exports
