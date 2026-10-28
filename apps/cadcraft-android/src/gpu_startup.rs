//! GPU acceleration startup and crash-safe backend selection for Android.
//!
//! Android devices expose hardware acceleration through **Vulkan 1.1+** (Adreno, Mali,
//! Immortalis, Xclipse, PowerVR) and **OpenGL ES 3.0+** (`wgpu::Backends::GL`).
//!
//! By default (`GpuBackend::Auto`, `RenderingMode::Auto` or `RenderingMode::Gpu`), CADCraft on
//! Android enables both `wgpu::Backends::VULKAN` and `wgpu::Backends::GL` with
//! `wgpu::PowerPreference::HighPerformance`, preferring hardware Vulkan first and falling back to
//! OpenGL ES if Vulkan surface creation or driver initialization fails. Full adapter buffer and
//! texture limits (`max_buffer_size` and `max_texture_dimension_2d`) are unlocked on the device
//! descriptor so large CAD drawings and clipped paper-space viewports render on the GPU without
//! hitting WebGPU's 256 MB / 2048 px default limits.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use eframe::{egui_wgpu, wgpu};
use serde_json::{Value, json};

/// Stencil bits requested on the window surface so clipped paper-space layout viewports are
/// clipped on the GPU in [`cadcraft_ui_egui::gpu`].
pub const STENCIL_BITS: u8 = 8;

/// Crash-recovery marker file name in the Android config directory.
pub const MARKER_FILE: &str = "gpu-starting.json";
const MARKER_MAX: u64 = 64 << 10;

/// GPU backend choice on Android.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GpuBackend {
    #[default]
    Auto,
    Vulkan,
    Gl,
    Cpu,
    Dx12,
    Metal,
}

impl GpuBackend {
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Vulkan => "vulkan",
            Self::Gl => "gl",
            Self::Cpu => "cpu",
            Self::Dx12 => "dx12",
            Self::Metal => "metal",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "vulkan" | "vk" => Some(Self::Vulkan),
            "gl" | "gles" | "opengl" => Some(Self::Gl),
            "cpu" | "software" => Some(Self::Cpu),
            "dx12" | "d3d12" => Some(Self::Dx12),
            "metal" | "mtl" => Some(Self::Metal),
            _ => None,
        }
    }
}

/// High-level rendering policy (`Auto` / `Gpu` enable hardware GPU canvas; `Cpu` forces CPU
/// canvas fallback).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RenderingMode {
    #[default]
    Auto,
    Gpu,
    Cpu,
}

impl RenderingMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "gpu" | "hardware" => Some(Self::Gpu),
            "cpu" | "software" => Some(Self::Cpu),
            _ => None,
        }
    }
}

/// Format a `wgpu::Backend` into a canonical lowercase name.
pub fn backend_name(backend: wgpu::Backend) -> &'static str {
    match backend {
        wgpu::Backend::Vulkan => "vulkan",
        wgpu::Backend::Gl => "gl",
        wgpu::Backend::Metal => "metal",
        wgpu::Backend::Dx12 => "dx12",
        wgpu::Backend::BrowserWebGpu => "webgpu",
        wgpu::Backend::Noop => "cpu",
    }
}

/// What a start records before touching the graphics driver.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Marker {
    pub backend: String,
    pub adapter: String,
    pub adapter_backend: String,
    pub driver: String,
    pub version: String,
}

impl Marker {
    pub fn to_json(&self) -> String {
        json!({
            "backend": self.backend,
            "adapter": self.adapter,
            "adapterBackend": self.adapter_backend,
            "driver": self.driver,
            "version": self.version,
        })
        .to_string()
    }

    pub fn parse(text: &str) -> Self {
        let v: Value = serde_json::from_str(text).unwrap_or(Value::Null);
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").chars().take(256).collect::<String>();
        Marker {
            backend: s("backend"),
            adapter: s("adapter"),
            adapter_backend: s("adapterBackend"),
            driver: s("driver"),
            version: s("version"),
        }
    }

    pub fn tried(&self) -> Option<GpuBackend> {
        let planned = if self.backend.is_empty() { Some(GpuBackend::Auto) } else { GpuBackend::parse(&self.backend) }?;
        match (planned, GpuBackend::parse(&self.adapter_backend)) {
            (GpuBackend::Auto, Some(actual)) => Some(actual),
            _ => Some(planned),
        }
    }
}

/// The GPU backend plan for this launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub backend: GpuBackend,
    pub env: Option<String>,
    pub reason: Option<String>,
    pub remember: bool,
}

/// Sanitize a backend preference for Android (DirectX 12 and Metal do not exist on Android and map
/// to `Auto`, which enables Vulkan + OpenGL ES).
pub fn sanitize(pref: GpuBackend) -> GpuBackend {
    match pref {
        GpuBackend::Dx12 | GpuBackend::Metal => GpuBackend::Auto,
        other => other,
    }
}

/// Fallback chain on Android: `Auto`/`Vulkan` -> `Gl` (OpenGL ES) -> `Cpu`.
pub fn next_safer(tried: GpuBackend) -> GpuBackend {
    match tried {
        GpuBackend::Auto | GpuBackend::Vulkan | GpuBackend::Dx12 | GpuBackend::Metal => GpuBackend::Gl,
        GpuBackend::Gl | GpuBackend::Cpu => GpuBackend::Cpu,
    }
}

/// Decide this launch's GPU backend from saved preferences, any crash marker, `WGPU_BACKEND`, and
/// `safe_gpu`.
pub fn plan(pref: GpuBackend, crashed: Option<&Marker>, env: Option<&str>, safe_gpu: bool) -> Plan {
    if let Some(v) = env.map(str::trim).filter(|v| !v.is_empty()) {
        let backend = if safe_gpu { GpuBackend::Cpu } else { GpuBackend::Auto };
        return Plan {
            backend,
            env: Some(v.to_string()),
            reason: Some(format!("WGPU_BACKEND={v} overrides the backend")),
            remember: false,
        };
    }
    if safe_gpu {
        return Plan {
            backend: GpuBackend::Cpu,
            env: None,
            reason: Some("safe-gpu: CPU renderer for this launch".into()),
            remember: false,
        };
    }
    let pref = sanitize(pref);
    if let Some((m, tried)) = crashed.and_then(|m| Some((m, m.tried()?))) {
        let next = next_safer(tried);
        let on = if m.adapter.is_empty() { String::new() } else { format!(" ({})", m.adapter) };
        return Plan {
            backend: next,
            env: None,
            reason: Some(format!("the previous start didn't finish on {}{on}; using {}", tried.name(), next.name())),
            remember: next != pref,
        };
    }
    Plan { backend: pref, env: None, reason: None, remember: false }
}

/// Resolve the rendering policy with [`RenderingMode`]. On Android, GPU acceleration is enabled by
/// default (`RenderingMode::Auto` or `RenderingMode::Gpu`).
pub fn plan_with_mode(pref: GpuBackend, mode: RenderingMode, crashed: Option<&Marker>, env: Option<&str>, safe_gpu: bool) -> Plan {
    let backend = match mode {
        RenderingMode::Cpu => GpuBackend::Cpu,
        RenderingMode::Auto | RenderingMode::Gpu if pref == GpuBackend::Cpu => GpuBackend::Auto,
        _ => pref,
    };
    let env = if safe_gpu || mode == RenderingMode::Cpu { None } else { env };
    let crashed = if mode == RenderingMode::Cpu { None } else { crashed };
    plan(backend, crashed, env, safe_gpu)
}

/// Persist hardware fallbacks (e.g. Vulkan -> OpenGL ES), but never permanently pin CPU mode.
pub fn backend_to_remember(plan: &Plan) -> Option<GpuBackend> {
    (plan.remember && plan.backend != GpuBackend::Cpu).then_some(plan.backend)
}

/// `wgpu::Backends` for Android:
/// - `Auto`: `VULKAN | GL` (hardware Vulkan with OpenGL ES 3.0+ fallback)
/// - `Vulkan`: `VULKAN`
/// - `Gl`: `GL`
/// - `Cpu`: `GL | VULKAN` (window surface presentation while canvas renders on CPU)
pub fn backends(plan: &Plan) -> Option<wgpu::Backends> {
    if plan.env.is_some() {
        return None;
    }
    Some(match plan.backend {
        GpuBackend::Auto | GpuBackend::Dx12 | GpuBackend::Metal => wgpu::Backends::VULKAN | wgpu::Backends::GL,
        GpuBackend::Vulkan => wgpu::Backends::VULKAN,
        GpuBackend::Gl => wgpu::Backends::GL,
        GpuBackend::Cpu => wgpu::Backends::GL | wgpu::Backends::VULKAN,
    })
}

/// Candidate adapter evaluated during startup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub vendor: u32,
    pub device_type: wgpu::DeviceType,
    pub backend: wgpu::Backend,
    pub surface_ok: bool,
}

fn rank(a: &Candidate, backend: GpuBackend) -> (u8, u8) {
    let ty = match a.device_type {
        wgpu::DeviceType::DiscreteGpu => 0,
        wgpu::DeviceType::IntegratedGpu => 1,
        wgpu::DeviceType::VirtualGpu => 2,
        wgpu::DeviceType::Other => 3,
        wgpu::DeviceType::Cpu => 4,
    };
    let ty = match (backend, a.device_type) {
        (GpuBackend::Cpu, wgpu::DeviceType::Cpu) => 0,
        (GpuBackend::Cpu, _) => ty + 1,
        _ => ty,
    };
    let be = match a.backend {
        wgpu::Backend::Vulkan => 0,
        wgpu::Backend::Gl => 1,
        _ => 2,
    };
    (ty, be)
}

/// Pick the best Android GPU adapter that can present to the surface.
pub fn pick(adapters: &[Candidate], backend: GpuBackend) -> Option<usize> {
    adapters
        .iter()
        .enumerate()
        .filter(|(_, a)| a.surface_ok)
        .min_by_key(|(i, a)| (rank(a, backend), *i))
        .map(|(i, _)| i)
}

/// Configure `eframe`'s `WgpuSetup` for Android GPU acceleration and unlock the hardware adapter's
/// full buffer and 2D texture limits for [`cadcraft_ui_egui::gpu`].
pub fn configure(setup: &mut egui_wgpu::WgpuSetup, plan: &Plan, sentinel: SharedSentinel, note: Arc<Mutex<Option<String>>>) {
    if let egui_wgpu::WgpuSetup::CreateNew(create) = setup {
        if let Some(b) = backends(plan) {
            create.instance_descriptor.backends = b;
        }
        create.power_preference = wgpu::PowerPreference::HighPerformance;
        let backend = plan.backend;
        create.native_adapter_selector = Some(Arc::new(move |adapters: &[wgpu::Adapter], surface: Option<&wgpu::Surface<'_>>| {
            let cands: Vec<Candidate> = adapters
                .iter()
                .map(|a| {
                    let i = a.get_info();
                    Candidate {
                        vendor: i.vendor,
                        device_type: i.device_type,
                        backend: i.backend,
                        surface_ok: surface.is_none_or(|s| a.is_surface_supported(s)),
                    }
                })
                .collect();
            let i = pick(&cands, backend).ok_or_else(|| "no compatible Android GPU adapter found".to_string())?;
            if backend == GpuBackend::Cpu && cands.get(i).is_some_and(|c| c.device_type != wgpu::DeviceType::Cpu) {
                *note.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some("CPU CAD canvas rendering; window presentation uses Android hardware graphics".into());
            }
            adapters.get(i).cloned().ok_or_else(|| "no compatible Android GPU adapter found".to_string())
        }));

        // Unlock the hardware adapter's full buffer size (for large CAD vertex/index meshes) and
        // 2D texture limits while recording the selected adapter in the startup sentinel.
        let default_limits = create.device_descriptor.clone();
        create.device_descriptor = Arc::new(move |adapter: &wgpu::Adapter| {
            let info = adapter.get_info();
            if let Some(s) = sentinel.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
                let mut m = s.marker.clone();
                m.adapter = info.name.clone();
                m.adapter_backend = backend_name(info.backend).to_string();
                m.driver = [info.driver.as_str(), info.driver_info.as_str()]
                    .iter()
                    .filter(|s| !s.is_empty())
                    .copied()
                    .collect::<Vec<_>>()
                    .join(" ");
                if let Err(e) = s.write(m) {
                    log::warn!("Android GPU startup marker: {e}");
                }
            }
            let mut desc = default_limits(adapter);
            let supported = adapter.limits();
            desc.required_limits.max_buffer_size = supported.max_buffer_size;
            desc.required_limits.max_texture_dimension_2d = supported.max_texture_dimension_2d;
            desc
        });
    }
}

/// Previous launch state from the marker file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Previous {
    Clean,
    Crashed(Marker),
    Busy,
}

impl Previous {
    pub fn crashed(&self) -> Option<&Marker> {
        match self {
            Previous::Crashed(m) => Some(m),
            _ => None,
        }
    }
}

/// Locked marker file for the active startup sequence.
#[derive(Debug)]
pub struct Sentinel {
    path: PathBuf,
    file: Option<File>,
    marker: Marker,
}

pub type SharedSentinel = Arc<Mutex<Option<Sentinel>>>;

impl Sentinel {
    pub fn begin(dir: &Path) -> (Previous, Option<Sentinel>) {
        let path = dir.join(MARKER_FILE);
        if let Err(e) = std::fs::create_dir_all(dir) {
            log::warn!("Android GPU startup marker dir: {e}");
            return (Previous::Clean, None);
        }
        let mut file = match OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path) {
            Ok(f) => f,
            Err(e) => {
                log::warn!("Android GPU startup marker {}: {e}", path.display());
                return (Previous::Clean, None);
            }
        };
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return (Previous::Busy, None),
            Err(std::fs::TryLockError::Error(e)) => {
                log::warn!("Android GPU startup marker lock: {e}");
                return (Previous::Clean, None);
            }
        }
        let mut text = String::new();
        let _ = (&mut file).take(MARKER_MAX).read_to_string(&mut text);
        let previous = if text.trim().is_empty() {
            Previous::Clean
        } else {
            Previous::Crashed(Marker::parse(&text))
        };
        (previous, Some(Sentinel { path, file: Some(file), marker: Marker::default() }))
    }

    pub fn write(&mut self, marker: Marker) -> std::io::Result<()> {
        let Some(file) = self.file.as_mut() else { return Ok(()) };
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(marker.to_json().as_bytes())?;
        file.sync_data()?;
        self.marker = marker;
        Ok(())
    }

    pub fn finish(mut self) {
        self.file.take();
        if let Err(e) = std::fs::remove_file(&self.path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("remove Android GPU startup marker {}: {e}", self.path.display());
        }
    }
}

/// Read GPU backend and rendering mode from the Android preferences JSON file.
pub fn read_rendering_prefs(prefs_file: Option<&Path>) -> (GpuBackend, RenderingMode) {
    let text = prefs_file.and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let backend = v
        .get("gpuBackend")
        .and_then(Value::as_str)
        .and_then(GpuBackend::parse)
        .unwrap_or(GpuBackend::Auto);
    let mode = v
        .get("renderingMode")
        .and_then(Value::as_str)
        .and_then(RenderingMode::parse)
        .unwrap_or(RenderingMode::Auto);
    (backend, mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn android_defaults_to_vulkan_and_gles_hardware_acceleration() {
        let p = plan_with_mode(GpuBackend::Auto, RenderingMode::Auto, None, None, false);
        assert_eq!(p.backend, GpuBackend::Auto);
        assert_eq!(backends(&p), Some(wgpu::Backends::VULKAN | wgpu::Backends::GL));
    }

    #[test]
    fn android_fallback_chain_steps_vulkan_to_gles_then_cpu() {
        let vk_crash = Marker {
            backend: "auto".into(),
            adapter: "Adreno (TM) 750".into(),
            adapter_backend: "vulkan".into(),
            driver: "Qualcomm".into(),
            version: "0.10.0".into(),
        };
        let p1 = plan_with_mode(GpuBackend::Auto, RenderingMode::Auto, Some(&vk_crash), None, false);
        assert_eq!(p1.backend, GpuBackend::Gl);
        assert_eq!(backends(&p1), Some(wgpu::Backends::GL));
        assert_eq!(backend_to_remember(&p1), Some(GpuBackend::Gl));

        let gl_crash = Marker {
            backend: "gl".into(),
            adapter: "Mali-G715".into(),
            adapter_backend: "gl".into(),
            driver: "ARM".into(),
            version: "0.10.0".into(),
        };
        let p2 = plan_with_mode(GpuBackend::Gl, RenderingMode::Auto, Some(&gl_crash), None, false);
        assert_eq!(p2.backend, GpuBackend::Cpu);
        assert_eq!(backend_to_remember(&p2), None, "CPU fallback is never permanently pinned");
    }

    #[test]
    fn adapter_picker_prefers_hardware_vulkan_over_gles_and_cpu() {
        let cands = [
            Candidate {
                vendor: 0x10005,
                device_type: wgpu::DeviceType::Cpu,
                backend: wgpu::Backend::Vulkan,
                surface_ok: true,
            },
            Candidate {
                vendor: 0x5143,
                device_type: wgpu::DeviceType::IntegratedGpu,
                backend: wgpu::Backend::Gl,
                surface_ok: true,
            },
            Candidate {
                vendor: 0x5143,
                device_type: wgpu::DeviceType::IntegratedGpu,
                backend: wgpu::Backend::Vulkan,
                surface_ok: true,
            },
        ];
        assert_eq!(pick(&cands, GpuBackend::Auto), Some(2));
    }
}
