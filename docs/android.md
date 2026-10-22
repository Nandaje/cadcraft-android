# CADCraft for Android (`cadcraft-android`)

`apps/cadcraft-android` and `packaging/android` port the full desktop CADCraft application (`cadcraft-ui-egui` + `cadcraft-engine` + `cadcraft-io` + `cadcraft-render`) to Android (`arm64-v8a`).

## 1. Hardware GPU Acceleration

GPU acceleration is enabled by default on Android (`apps/cadcraft-android/src/gpu_startup.rs` and `apps/cadcraft-android/src/shell.rs`):

- **Vulkan 1.1+ Primary Backend & OpenGL ES 3.0+ Fallback**: Configures `wgpu::Backends::VULKAN | wgpu::Backends::GL` with `wgpu::PowerPreference::HighPerformance`. Hardware Vulkan adapters (Qualcomm Adreno, ARM Mali/Immortalis, Samsung Xclipse, PowerVR) are ranked highest, with automatic fallback to OpenGL ES 3.0+ (`wgpu::Backend::Gl`).
- **Full Hardware Buffer & Texture Limits**: `gpu_startup::configure` unlocks the Android GPU's native `max_buffer_size` and `max_texture_dimension_2d` limits on the `wgpu::DeviceDescriptor` so large CAD drawings and viewports render directly on the GPU (`cadcraft_ui_egui::gpu`).
- **8-Bit Stencil Buffer for Layout Viewports**: Requests `stencil_buffer: 8` (`STENCIL_BITS = 8`) and passes `depth_format_from_bits(0, STENCIL_BITS)` to `CadApp::set_wgpu` so clipped paper-space viewports (rectangular, polygonal, rotated) are hardware-clipped on the GPU.
- **Crash-Safe Startup Sentinel**: Writes `gpu-starting.json` before initializing the Android graphics driver and clears it after the first frame completes. If a buggy mobile driver crashes during initialization, the next launch automatically steps down `Vulkan` -> `Gl` -> `Cpu`.

## 2. File & Storage Permissions for Import and Export

Android storage permissions and file dialogs are implemented across `apps/cadcraft-android/src/permissions.rs`, `apps/cadcraft-android/src/file_dialog.rs`, `apps/cadcraft-android/src/android_entry.rs`, and `packaging/android/app/src/main/java/ai/storyteller/cadcraft/MainActivity.kt`:

- **Manifest & Runtime Permissions**:
  - `android.permission.READ_EXTERNAL_STORAGE` (Android 8–12L, API 26–32)
  - `android.permission.WRITE_EXTERNAL_STORAGE` (Android 8–10, API 26–29)
  - `android.permission.READ_MEDIA_IMAGES` (Android 13+, API 33+)
  - `android.permission.MANAGE_EXTERNAL_STORAGE` (Android 11+, API 30+ All Files Access for CAD project folders, `.dxf`, `.dwg`, `.dwt`, external references/xrefs, `.ctb`/`.stb` plot style tables, `.lin` linetypes, `.pat` hatch patterns, and `.lsp` AutoLISP scripts)
- **Dual File Picker Support**:
  1. **In-App Touch File Browser & Save/Export Modal** (`file_dialog::render_modal`): Browse `Downloads`, `Documents`, `Pictures`, `Shared Storage`, `App External`, `App Exports`, and `App Internal`; create folders; filter by CAD extensions (`.dxf`, `.dwg`, `.dwt`, `.svg`, `.svgz`, `.lsp`, `.scr`, `.lin`, `.pat`, `.ctb`, `.stb`); and choose save/export/plot formats (`DXF`, `DWG`, `DWT`, `SVG`, `PDF`, `PNG`).
  2. **Android Storage Access Framework (SAF)** (`MainActivity.kt`): Launch the native system file picker (`ACTION_OPEN_DOCUMENT`), save dialog (`ACTION_CREATE_DOCUMENT`), or folder picker (`ACTION_OPEN_DOCUMENT_TREE`), plus handle incoming `ACTION_VIEW` / `ACTION_EDIT` / `ACTION_SEND` intents.
- **Scoped-Storage Safe Export & MediaScanner**: `permissions::resolve_export_target` checks directory writability before saving/exporting and automatically falls back to `/storage/emulated/0/Download` or `<external_data>/Exports` when Android scoped storage blocks a path, then triggers `MediaScannerConnection.scanFile` (and flushes any SAF `content://` `OutputStream`) so exported drawings, plots, and PDFs are immediately accessible.
