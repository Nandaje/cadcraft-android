//! CADCraft for Android (`libcadcraft_android.so`).
//!
//! This crate ports the desktop CADCraft application (`cadcraft-ui-egui` + `cadcraft-engine` +
//! `cadcraft-io` + `cadcraft-render`) to Android with:
//! 1. **Hardware GPU Acceleration** enabled by default (`Vulkan` primary backend with `OpenGL ES`
//!    fallback via `wgpu`, full adapter buffer and texture limits unlocked, 8-bit stencil buffer
//!    for clipped paper-space layout viewports, and `CadApp::set_wgpu` GPU canvas rendering).
//! 2. **File & Media Permissions for Import and Export** (`READ_EXTERNAL_STORAGE`,
//!    `WRITE_EXTERNAL_STORAGE`, `READ_MEDIA_IMAGES`, `MANAGE_EXTERNAL_STORAGE`, plus Android
//!    Storage Access Framework `ACTION_OPEN_DOCUMENT` / `ACTION_CREATE_DOCUMENT` integration and a
//!    touch-friendly in-app Android File Browser modal).

pub mod file_dialog;
pub mod gpu_startup;
pub mod permissions;
pub mod services;
pub mod shell;
pub mod touch_twins;

#[cfg(target_os = "android")]
pub mod android_entry;

/// Exported symbol on non-Android host builds so Windows/Linux `cdylib` linkers always have at
/// least one exported symbol in the dynamic library export table.
#[cfg(not(target_os = "android"))]
#[allow(unsafe_code)]
// SAFETY: Pure constant function with no arguments or state; exported solely so host `cargo test`
// with `crate-type = ["cdylib", "rlib"]` links cleanly on Windows GNU toolchains.
#[unsafe(no_mangle)]
pub extern "C" fn cadcraft_android_abi_version() -> u32 {
    1
}
