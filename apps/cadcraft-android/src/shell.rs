//! Android shell wrapping [`cadcraft_ui_egui::CadApp`] with touch-twin filtering, hardware GPU
//! canvas activation (including 8-bit stencil buffer for clipped paper-space viewports), incoming
//! file intent handling, and the Android file/permission modal overlay.

use std::sync::{Arc, Mutex, PoisonError};

use cadcraft_engine::Session;
use cadcraft_ui_egui::{CadApp, PickerKind};
use serde_json::json;

use crate::file_dialog::{self, CompletedFileAction};
use crate::gpu_startup::{self, GpuBackend, Plan, RenderingMode, STENCIL_BITS, SharedSentinel};
use crate::services;
use crate::touch_twins;

/// The `eframe::App` wrapper for CADCraft on Android.
pub struct AndroidShell {
    pub app: CadApp,
    started_sentinel: SharedSentinel,
    remember_backend: Option<GpuBackend>,
    first_frame_done: bool,
    last_saved_prefs: String,
    last_saved_profile: String,
    last_saved_autosave: String,
}

impl AndroidShell {
    /// Construct the Android shell inside `eframe::run_native`'s creation callback, enabling the
    /// `wgpu` GPU canvas (`CadApp::set_wgpu`) when hardware graphics are available.
    pub fn create(
        cc: &eframe::CreationContext<'_>,
        plan: &Plan,
        mode: RenderingMode,
        started_sentinel: SharedSentinel,
        gpu_note: Arc<Mutex<Option<String>>>,
    ) -> Self {
        services::install_cadcraft_io();
        services::init_support_dirs();

        let svcs = services::android_services();
        let mut app = CadApp::new(Session::new(), svcs);
        app.integrated_titlebar = false;
        app.session.set_lisp_support_dir(Some(services::lisp_support_dir()));

        // Restore persisted preferences, profile (Options), and autosave settings.
        if let Ok(prefs) = std::fs::read_to_string(services::prefs_file()) {
            app.load_prefs(&prefs);
        }
        if let Ok(profile) = std::fs::read_to_string(services::profile_file()) {
            app.load_profile(&profile);
        }
        if let Ok(auto) = std::fs::read_to_string(services::autosave_prefs_file()) {
            cadcraft_ui_egui::autosave::load_prefs(&auto);
        }

        // Enable crash-recovery autosave in the Android app-internal Autosave directory.
        app.enable_autosave(&services::autosave_dir());

        if let Some(rs) = cc.wgpu_render_state.as_ref() {
            let adapter_info = rs.adapter.get_info();
            log::info!(
                "cadcraft-android: wgpu adapter {:?} ({:?}, backend {:?})",
                adapter_info.name,
                adapter_info.device_type,
                adapter_info.backend
            );
            let software_window = adapter_info.device_type == eframe::wgpu::DeviceType::Cpu;
            if !software_window
                && plan.backend != GpuBackend::Cpu
                && mode != RenderingMode::Cpu
                && std::env::var_os("CADCRAFT_CPU_CANVAS").is_none()
            {
                let stencil_format = eframe::egui_wgpu::depth_format_from_bits(0, STENCIL_BITS);
                app.set_wgpu(rs, stencil_format);
            } else if let Some(note) = gpu_note.lock().unwrap_or_else(PoisonError::into_inner).clone() {
                app.set_status(note);
            }
        }

        if let Some(reason) = &plan.reason {
            app.session.echo(format!("GPU startup: {reason}"));
        }

        let last_saved_prefs = app.prefs_json();
        let last_saved_profile = app.profile_json();
        let last_saved_autosave = cadcraft_ui_egui::autosave::prefs_json();

        Self {
            app,
            started_sentinel,
            remember_backend: gpu_startup::backend_to_remember(plan),
            first_frame_done: false,
            last_saved_prefs,
            last_saved_profile,
            last_saved_autosave,
        }
    }

    fn drain_incoming_files(&mut self) {
        let incoming: Vec<(String, Vec<u8>)> = {
            let inbox = services::global_inbox();
            let mut guard = inbox.lock().unwrap_or_else(PoisonError::into_inner);
            std::mem::take(&mut *guard)
        };
        for (name, bytes) in incoming {
            self.app.open_bytes(&name, &bytes);
        }
    }

    fn drain_file_dialog_actions(&mut self, ctx: &egui::Context) {
        for action in file_dialog::take_completed_actions() {
            match action {
                CompletedFileAction::OpenPath { kind, path } => {
                    self.execute_open_path(kind, &path);
                }
                CompletedFileAction::OpenBytes { kind, name, bytes } => {
                    if kind == PickerKind::Open {
                        self.app.open_bytes(&name, &bytes);
                    } else {
                        match services::stage_saf_bytes(&name, &bytes) {
                            Ok(staged) => self.execute_open_path(kind, &staged.to_string_lossy()),
                            Err(e) => self.app.open_failed(e),
                        }
                    }
                }
                CompletedFileAction::SavePath {
                    kind,
                    path,
                    fallback_notice,
                } => {
                    self.execute_save_path(ctx, kind, &path, fallback_notice);
                }
            }
        }
    }

    fn execute_open_path(&mut self, kind: PickerKind, path: &str) {
        match kind {
            PickerKind::Open => self.app.open_path(std::path::Path::new(path)),
            PickerKind::NewTemplate => {
                if let Err(e) = self.app.run("new", json!({ "template": path })) {
                    self.app.set_status(e);
                } else {
                    self.app.ui.start_tab = false;
                    self.app.canvas.zoom_pending = true;
                }
            }
            PickerKind::Recover => {
                match self.app.run("recover", json!({ "path": path })) {
                    Ok(_) => {
                        self.app.ui.start_tab = false;
                        self.app.canvas.zoom_pending = true;
                    }
                    Err(e) => self.app.set_status(e),
                }
            }
            PickerKind::Import => {
                match self.app.run("import", json!({ "path": path })) {
                    Ok(_) => {
                        self.app.set_status(format!("Imported {path}"));
                        self.app.canvas.zoom_pending = true;
                    }
                    Err(e) => self.app.set_status(e),
                }
            }
            PickerKind::XAttach => {
                if self.app.session.start("xattach").is_ok() {
                    let _ = self.app.session.input(cadcraft_engine::Input::Text(path.to_string()));
                }
            }
            PickerKind::Linetype => {
                if let Err(e) = self.app.run("linetype", json!({ "load": "*", "file": path })) {
                    self.app.set_status(e);
                }
            }
            PickerKind::AppLoad => {
                if let Some(dir) = std::path::Path::new(path)
                    .parent()
                    .and_then(std::path::Path::to_str)
                    .filter(|d| !d.is_empty())
                {
                    self.app.ui.last_lisp_dir = Some(dir.to_string());
                }
                if self.app.session.start("appload").is_ok() {
                    self.app.cmdline(path);
                }
            }
            _ => self.app.open_path(std::path::Path::new(path)),
        }
    }

    fn execute_save_path(&mut self, ctx: &egui::Context, kind: PickerKind, path: &str, fallback_notice: Option<String>) {
        if kind == PickerKind::Plot {
            file_dialog::set_ready_plot_save_path(path.to_string());
            cadcraft_ui_egui::resume_plot_save(ctx);
            file_dialog::notify_media_scanner(path);
            if let Some(note) = fallback_notice {
                self.app.session.echo(note.clone());
                self.app.set_status(note);
            }
            return;
        }

        let ext = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();

        let result = match kind {
            PickerKind::SaveAs if matches!(ext.as_str(), "svg" | "png" | "pdf") => {
                self.app.run("export", json!({ "path": path }))
            }
            PickerKind::SaveAs => {
                let params = cadcraft_ui_egui::menus::saveas_params(&self.app.ui, &self.app.session, path);
                self.app.run("saveas", params)
            }
            PickerKind::Export if matches!(ext.as_str(), "dxf" | "dwg" | "dwt") => {
                let params = cadcraft_ui_egui::menus::saveas_params(&self.app.ui, &self.app.session, path);
                self.app.run("saveas", params)
            }
            PickerKind::Export => self.app.run("export", json!({ "path": path })),
            PickerKind::ExportPdf => self.app.run("exportpdf", json!({ "path": path })),
            PickerKind::Publish => self.app.run("publish", json!({ "path": path })),
            _ => self.app.run("export", json!({ "path": path })),
        };

        match result {
            Ok(_) => {
                file_dialog::notify_media_scanner(path);
                let msg = fallback_notice.unwrap_or_else(|| format!("Saved to {path}"));
                self.app.session.echo(msg.clone());
                self.app.set_status(msg);
            }
            Err(e) => {
                self.app.set_status(e);
            }
        }
    }

    fn persist_settings_if_changed(&mut self) {
        let prefs = self.app.prefs_json();
        if prefs != self.last_saved_prefs {
            let _ = std::fs::write(services::prefs_file(), &prefs);
            self.last_saved_prefs = prefs;
        }
        let profile = self.app.profile_json();
        if profile != self.last_saved_profile {
            let _ = std::fs::write(services::profile_file(), &profile);
            self.last_saved_profile = profile;
        }
        let auto = cadcraft_ui_egui::autosave::prefs_json();
        if auto != self.last_saved_autosave {
            let _ = std::fs::write(services::autosave_prefs_file(), &auto);
            self.last_saved_autosave = auto;
        }
    }
}

impl eframe::App for AndroidShell {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        // Deduplicate simultaneous pointer + touch events on Android touchscreens so a single tap
        // on a menu item does not open and immediately close the menu.
        touch_twins::drop_twins(ctx, raw_input);
        self.app.raw_input_hook(ctx, raw_input);
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_incoming_files();
        self.drain_file_dialog_actions(ctx);
        self.app.logic(ctx);

        if !self.first_frame_done {
            self.first_frame_done = true;
            if let Some(s) = self.started_sentinel.lock().unwrap_or_else(PoisonError::into_inner).take() {
                s.finish();
            }
            if let Some(b) = self.remember_backend.take() {
                log::info!("cadcraft-android: remembering fallback GPU backend {}", b.name());
            }
        }

        self.persist_settings_if_changed();

        if self.app.quit_requested {
            self.app.quit_requested = false;
            if !self.app.request_quit() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.app.ui(ui);
        file_dialog::render_modal(ui.ctx());
    }

    fn on_exit(&mut self) {
        self.persist_settings_if_changed();
        self.app.shutdown_autosave();
    }
}
