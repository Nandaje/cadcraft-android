# CADCraft for Android Packaging

This directory contains the Android Gradle project (`ai.storyteller.cadcraft`) and helper script (`package.sh`) that bundle `apps/cadcraft-android` (`libcadcraft_android.so`) into a standalone Android APK.

## Key Capabilities

1. **Hardware GPU Acceleration Enabled by Default**:
   - Requests `wgpu::Backends::VULKAN | wgpu::Backends::GL` with `wgpu::PowerPreference::HighPerformance` (`apps/cadcraft-android/src/gpu_startup.rs`).
   - Prefers hardware **Vulkan 1.1+** (Adreno, Mali, Immortalis, Xclipse, PowerVR) and falls back automatically to **OpenGL ES 3.0+** (`wgpu::Backends::GL`).
   - Unlocks the hardware adapter's full `max_buffer_size` and `max_texture_dimension_2d` limits and configures an 8-bit stencil buffer (`STENCIL_BITS = 8`) for clipped paper-space layout viewports in `cadcraft_ui_egui::gpu`.

2. **Full File & Media Permissions for Import and Export**:
   - Declares `READ_EXTERNAL_STORAGE` (API 26–32), `WRITE_EXTERNAL_STORAGE` (API 26–29), `READ_MEDIA_IMAGES` (API 33+), and `MANAGE_EXTERNAL_STORAGE` (API 30+ All Files Access for CAD project folders, `.dxf`, `.dwg`, `.dwt`, external references/xrefs, `.ctb`/`.stb` plot styles, `.lin` linetypes, `.pat` hatches, and `.lsp` AutoLISP scripts).
   - Includes a touch-friendly in-app **Android File Browser & Save/Export Modal** (`apps/cadcraft-android/src/file_dialog.rs`) with quick bookmarks (`Downloads`, `Documents`, `Pictures`, `Shared Storage`, `App External`, `App Exports`, `App Internal`) plus direct JNI hooks to the Android **Storage Access Framework (SAF)** system file/save/folder pickers (`ACTION_OPEN_DOCUMENT`, `ACTION_CREATE_DOCUMENT`, `ACTION_OPEN_DOCUMENT_TREE`).
   - Automatically indexes saved/exported files via `MediaScannerConnection.scanFile` and flushes staged SAF saves to `content://` `OutputStream`s.

## Building

```bash
rustup target add aarch64-linux-android
cargo install cargo-ndk --locked
./packaging/android/package.sh
```
