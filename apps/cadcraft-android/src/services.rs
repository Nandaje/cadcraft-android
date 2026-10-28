//! Android platform services: CAD file import/export (`DXF`, `DWG`, `DWT`, `SVG`, `SVGZ`, `PDF`,
//! `PNG`), permission-aware file writing, SAF staging, autosave, preferences, and LISP support
//! directories.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use cadcraft_ui_egui::{PickerKind, Services};

use crate::file_dialog;
use crate::permissions;

/// Thread-safe queue of `(file_name, file_bytes)` delivered from Android `ACTION_VIEW` /
/// `ACTION_EDIT` / `ACTION_SEND` intents.
pub type Inbox = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

static GLOBAL_INBOX: OnceLock<Inbox> = OnceLock::new();

pub fn global_inbox() -> Inbox {
    GLOBAL_INBOX.get_or_init(|| Arc::new(Mutex::new(Vec::new()))).clone()
}

/// Enqueue an incoming CAD drawing or LISP script from an Android Intent (`MainActivity`).
pub fn enqueue_incoming_file(name: String, bytes: Vec<u8>) {
    global_inbox().lock().unwrap_or_else(PoisonError::into_inner).push((name, bytes));
}

/// Per-user configuration directory on Android (`internal_data` from `AndroidApp`, or
/// `CADCRAFT_CONFIG_DIR`).
pub fn config_dir() -> PathBuf {
    permissions::current_dirs().internal_data
}

pub fn prefs_file() -> PathBuf {
    config_dir().join("preferences.json")
}

pub fn profile_file() -> PathBuf {
    config_dir().join("profile.json")
}

pub fn autosave_prefs_file() -> PathBuf {
    config_dir().join("autosave.json")
}

pub fn aliases_file() -> PathBuf {
    config_dir().join("aliases.pgp")
}

pub fn autosave_dir() -> PathBuf {
    config_dir().join("Autosave")
}

pub fn lisp_support_dir() -> PathBuf {
    config_dir().join("support")
}

/// Register the `cadcraft-io` DXF/DWG/SVG/PDF/PNG readers, writers, entity codec, and plotter
/// callbacks with `cadcraft-engine`.
pub fn install_cadcraft_io() {
    cadcraft_engine::cmd::file::set_io(cadcraft_engine::cmd::file::IoHooks {
        read: |b, name| cadcraft_io::read(b, name).map_err(|e| e.to_string()),
        write: |d, name| cadcraft_io::write(d, name).map_err(|e| e.to_string()),
        plot: Some(|d, space, opts| cadcraft_io::plotter::plot(d, space, opts).map_err(|e| e.to_string())),
    });
    cadcraft_engine::cmd::file::set_write_as(|d, name, o| {
        cadcraft_io::write_with(
            d,
            name,
            &cadcraft_io::SaveOptions {
                version: o.version,
                binary: o.binary,
            },
        )
        .map_err(|e| e.to_string())
    });
    cadcraft_engine::cmd::file::set_entity_codec(cadcraft_engine::cmd::file::EntityCodec {
        groups: cadcraft_io::entity_groups,
        entity: cadcraft_io::entity_from_groups,
    });
}

/// Initialize Android support directories for LISP `(findfile ...)` / `(load ...)` and autosave.
pub fn init_support_dirs() {
    let _ = std::fs::create_dir_all(autosave_dir());
    let _ = std::fs::create_dir_all(lisp_support_dir());
}

/// Stage file bytes returned from Android SAF (`content://` URI) into the app's writable `Imports`
/// directory so path-based CAD commands (`import`, `newtemplate`, `recover`, `xattach`, `linetype`)
/// can read the file by path.
pub fn stage_saf_bytes(name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let dirs = permissions::current_dirs();
    let stage_dir = dirs.imports_staging_dir();
    std::fs::create_dir_all(&stage_dir).map_err(|e| format!("create staging dir {}: {e}", stage_dir.display()))?;
    let safe_name: String = Path::new(name)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "imported.dxf".into());
    let dest = stage_dir.join(safe_name);
    std::fs::write(&dest, bytes).map_err(|e| format!("write staged file {}: {e}", dest.display()))?;
    Ok(dest)
}

/// Write `bytes` to `path` with Android scoped-storage fallback and `MediaScannerConnection`
/// indexing.
pub fn write_with_android_permissions(path: &Path, bytes: &[u8]) -> Result<(PathBuf, Option<String>), String> {
    let dirs = permissions::current_dirs();
    let (target, fallback_note) = permissions::resolve_export_target(path, &dirs);
    if let Some(parent) = target.parent().filter(|p| !p.as_os_str().is_empty()) {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&target, bytes).map_err(|e| format!("{}: {e}", target.display()))?;
    if let Some(target_str) = target.to_str() {
        file_dialog::notify_media_scanner(target_str);
    }
    Ok((target, fallback_note))
}

/// Construct the Android [`Services`] table for [`cadcraft_ui_egui::CadApp`].
pub fn android_services() -> Services {
    Services {
        pick_open: Some(Box::new(|| {
            let kind = cadcraft_ui_egui::last_picker_kind().unwrap_or(PickerKind::Open);
            file_dialog::open_file_dialog(kind);
            None
        })),
        pick_save: Some(Box::new(|suggested: &str| {
            let kind = cadcraft_ui_egui::last_picker_kind().unwrap_or(PickerKind::SaveAs);
            if kind == PickerKind::Plot
                && let Some(ready) = file_dialog::take_ready_plot_save_path()
            {
                return Some(ready);
            }
            file_dialog::save_file_dialog(kind, suggested);
            None
        })),
        pick_lisp: Some(Box::new(|_dir: &str| {
            file_dialog::open_file_dialog(PickerKind::AppLoad);
            None
        })),
        no_file_system: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadcraft_engine::Session;
    use serde_json::json;

    #[test]
    fn android_services_roundtrip_dxf_dwg_svg_pdf_png_with_permissions() {
        install_cadcraft_io();

        let dir = std::env::temp_dir().join(format!("cadcraft-android-svc-test-{}", std::process::id()));
        let internal = dir.join("internal");
        let external = dir.join("external");
        let shared = dir.join("shared");
        let downloads = shared.join("Download");
        let _ = std::fs::create_dir_all(&downloads);
        permissions::init_dirs(Some(internal), Some(external));
        init_support_dirs();

        let mut session = Session::new();
        session
            .execute("line", &json!({ "points": [[0.0, 0.0], [100.0, 50.0]] }))
            .unwrap();
        session
            .execute("circle", &json!({ "center": [50.0, 25.0], "radius": 20.0 }))
            .unwrap();

        // 1. Save / Export DXF, DWG, SVG, PDF, and PNG to Android Downloads directory.
        for ext in ["dxf", "dwg", "svg", "pdf", "png"] {
            let out_path = downloads.join(format!("android_part.{ext}"));
            let (resolved, note) = permissions::resolve_export_target(&out_path, &permissions::current_dirs());
            assert_eq!(resolved, out_path);
            assert!(note.is_none());

            let cmd = if ext == "dxf" || ext == "dwg" { "saveas" } else { "export" };
            session
                .execute(cmd, &json!({ "path": resolved.to_string_lossy() }))
                .unwrap();
            assert!(resolved.is_file(), "expected exported {ext} file at {}", resolved.display());
            let metadata = std::fs::metadata(&resolved).unwrap();
            assert!(metadata.len() > 0, "exported {ext} file should be non-empty");
        }

        // 2. Re-open exported DXF and DWG and import SVG into a fresh Session.
        let mut verify_session = Session::new();
        verify_session
            .execute("open", &json!({ "path": downloads.join("android_part.dxf").to_string_lossy() }))
            .unwrap();
        assert_eq!(verify_session.doc().unwrap().model.len(), 2);

        verify_session
            .execute("open", &json!({ "path": downloads.join("android_part.dwg").to_string_lossy() }))
            .unwrap();
        assert_eq!(verify_session.doc().unwrap().model.len(), 2);

        verify_session
            .execute("import", &json!({ "path": downloads.join("android_part.svg").to_string_lossy() }))
            .unwrap();
        assert!(verify_session.doc().unwrap().model.len() >= 2);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
