//! Android NativeActivity / JNI entrypoint (`android_main` and JNI callbacks for `MainActivity`).
//!
//! Only this module uses `unsafe` (for the `#[unsafe(no_mangle)]` FFI exports required by
//! `android-activity` and the Android NDK JNI runtime). Every `unsafe` block carries an explicit
//! `SAFETY:` justification.

#![allow(unsafe_code)]

use std::sync::{Arc, Mutex, PoisonError};

use android_activity::AndroidApp;
use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{JByteArray, JClass, JObject, JString, JValue};
use jni::sys::{jboolean, jint};
use jni::{EnvUnowned, JavaVM, jni_sig, jni_str};

use crate::file_dialog::{self, PlatformBridge};
use crate::gpu_startup;
use crate::permissions;
use crate::services;
use crate::shell::AndroidShell;

const APP_ID: &str = "ai.storyteller.cadcraft";

/// Invoke a method on the Java `MainActivity` on the Android UI thread via `AndroidApp`.
fn call_activity_on_ui_thread(
    app: &AndroidApp,
    f: impl FnOnce(&mut jni::Env<'_>, &JObject<'_>) -> jni::errors::Result<()> + Send + 'static,
) {
    let app_clone = app.clone();
    app.run_on_java_main_thread(Box::new(move || {
        let vm_ptr: *mut jni::sys::JavaVM = app_clone.vm_as_ptr().cast();
        if vm_ptr.is_null() {
            return;
        }
        // SAFETY: `AndroidApp::vm_as_ptr` returns the process-wide `JavaVM*` provided by the
        // Android NDK runtime, which remains valid for the lifetime of the JVM.
        let vm = unsafe { JavaVM::from_raw(vm_ptr) };
        let act_raw = app_clone.activity_as_ptr() as jni::sys::jobject;
        if act_raw.is_null() {
            return;
        }
        if let Err(err) = vm.attach_current_thread(|env| -> jni::errors::Result<()> {
            // SAFETY: `act_raw` is a valid JNI object reference to the running `Activity` for the
            // duration of `run_on_java_main_thread`. `as_cast_raw` borrows it without deleting the
            // reference on drop.
            let activity = unsafe { env.as_cast_raw::<JObject<'_>>(&act_raw)? };
            f(env, &activity)
        }) {
            log::warn!("cadcraft-android: JNI call to MainActivity failed or method not present: {err:?}");
        }
    }));
}

fn install_jni_bridge(app: &AndroidApp) {
    let app_perm = app.clone();
    let app_open = app.clone();
    let app_save = app.clone();
    let app_folder = app.clone();
    let app_scan = app.clone();

    file_dialog::set_platform_bridge(PlatformBridge {
        request_permissions: Box::new(move |all_files: bool| {
            call_activity_on_ui_thread(&app_perm, move |env, activity| {
                env.call_method(
                    activity,
                    jni_str!("requestStoragePermissions"),
                    jni_sig!((all_files: jboolean) -> ()),
                    &[JValue::Bool(all_files)],
                )?;
                Ok(())
            });
        }),
        launch_saf_open: Box::new(move |multiple: bool| {
            call_activity_on_ui_thread(&app_open, move |env, activity| {
                env.call_method(
                    activity,
                    jni_str!("openSafFilePicker"),
                    jni_sig!((multiple: jboolean) -> ()),
                    &[JValue::Bool(multiple)],
                )?;
                Ok(())
            });
        }),
        launch_saf_save: Box::new(move |suggested_name: &str| {
            let name = suggested_name.to_string();
            call_activity_on_ui_thread(&app_save, move |env, activity| {
                let jname = env.new_string(&name)?;
                env.call_method(
                    activity,
                    jni_str!("openSafSavePicker"),
                    jni_sig!((suggested: JString) -> ()),
                    &[JValue::Object(&jname)],
                )?;
                Ok(())
            });
        }),
        launch_saf_folder: Box::new(move || {
            call_activity_on_ui_thread(&app_folder, move |env, activity| {
                env.call_method(
                    activity,
                    jni_str!("openSafFolderPicker"),
                    jni_sig!(() -> ()),
                    &[],
                )?;
                Ok(())
            });
        }),
        scan_media_file: Box::new(move |path: &str| {
            let path = path.to_string();
            call_activity_on_ui_thread(&app_scan, move |env, activity| {
                let jpath = env.new_string(&path)?;
                env.call_method(
                    activity,
                    jni_str!("scanMediaFile"),
                    jni_sig!((path: JString) -> ()),
                    &[JValue::Object(&jpath)],
                )?;
                Ok(())
            });
        }),
    });
}

/// Main entrypoint invoked by `android-activity` when the Android `Activity` starts.
// SAFETY: `android_main` is the required unmangled entrypoint symbol expected by `android-activity`.
#[unsafe(no_mangle)]
pub fn android_main(app: AndroidApp) {
    // Enable stylus / S-Pen pressure, tilt, orientation and hover distance axes on Android.
    app.enable_motion_axis(android_activity::input::Axis::Pressure);
    app.enable_motion_axis(android_activity::input::Axis::Tilt);
    app.enable_motion_axis(android_activity::input::Axis::Orientation);
    app.enable_motion_axis(android_activity::input::Axis::Distance);

    permissions::init_dirs(app.internal_data_path(), app.external_data_path());
    install_jni_bridge(&app);

    // Request runtime file/media storage permissions on startup if not yet granted.
    file_dialog::request_storage_permissions(false);

    let prefs_path = services::prefs_file();
    let (pref, mode) = gpu_startup::read_rendering_prefs(Some(&prefs_path));
    let config_dir = services::config_dir();
    let (previous, sentinel) = gpu_startup::Sentinel::begin(&config_dir);
    let env_backend = std::env::var("WGPU_BACKEND").ok();
    let safe_gpu = std::env::var_os("CADCRAFT_SAFE_GPU").is_some();
    let plan = gpu_startup::plan_with_mode(pref, mode, previous.crashed(), env_backend.as_deref(), safe_gpu);

    let sentinel: gpu_startup::SharedSentinel = Arc::new(Mutex::new(sentinel));
    if let Some(s) = sentinel.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
        let backend = match &plan.env {
            Some(v) => format!("env:{v}"),
            None => plan.backend.name().to_string(),
        };
        let marker = gpu_startup::Marker {
            backend,
            version: env!("CARGO_PKG_VERSION").to_string(),
            ..Default::default()
        };
        if let Err(e) = s.write(marker) {
            log::warn!("Android GPU startup marker: {e}");
        }
    }

    let gpu_note: Arc<Mutex<Option<String>>> = Arc::default();
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_app_id(APP_ID)
            .with_title("CADCraft")
            .with_drag_and_drop(true),
        stencil_buffer: gpu_startup::STENCIL_BITS,
        persistence_path: Some(config_dir.join("ui.ron")),
        android_app: Some(app),
        ..Default::default()
    };

    gpu_startup::configure(&mut options.wgpu_options.wgpu_setup, &plan, sentinel.clone(), gpu_note.clone());
    options.wgpu_options.surface.present_mode = eframe::wgpu::PresentMode::AutoVsync;

    let started_sentinel = sentinel.clone();

    let result = eframe::run_native(
        "CADCraft",
        options,
        Box::new(move |cc| {
            let shell = AndroidShell::create(cc, &plan, mode, started_sentinel, gpu_note);
            Ok(Box::new(shell))
        }),
    );

    if let Err(e) = result {
        log::error!("cadcraft-android exited with error: {e}");
    }
}

/// JNI callback invoked by `MainActivity` when storage permissions are checked or granted.
// SAFETY: Standard JNI export signature for `ai.storyteller.cadcraft.MainActivity.nativeOnStoragePermissionResult`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_storyteller_cadcraft_MainActivity_nativeOnStoragePermissionResult<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    read_granted: jboolean,
    write_granted: jboolean,
    all_files_access: jboolean,
    sdk_int: jint,
) {
    let outcome = unowned_env.with_env(|_env| -> Result<(), jni::errors::Error> {
        permissions::set_permission_status(read_granted, write_granted, all_files_access, sdk_int);
        permissions::refresh_permission_probe();
        Ok(())
    });
    outcome.resolve::<ThrowRuntimeExAndDefault>();
}

/// JNI callback invoked by `MainActivity` when a file is opened via `Intent.ACTION_VIEW`,
/// `Intent.ACTION_EDIT`, or `Intent.ACTION_SEND`.
// SAFETY: Standard JNI export signature for `ai.storyteller.cadcraft.MainActivity.nativeDeliverFile`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_storyteller_cadcraft_MainActivity_nativeDeliverFile<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    file_name: JString<'caller>,
    file_bytes: JByteArray<'caller>,
) {
    let outcome = unowned_env.with_env(|env| -> Result<(), jni::errors::Error> {
        let name = file_name.to_string();
        let bytes = env.convert_byte_array(&file_bytes)?;
        services::enqueue_incoming_file(name, bytes);
        Ok(())
    });
    outcome.resolve::<ThrowRuntimeExAndDefault>();
}

/// JNI callback invoked by `MainActivity` when the user picks a file in the Android SAF
/// `ACTION_OPEN_DOCUMENT` dialog.
// SAFETY: Standard JNI export signature for `ai.storyteller.cadcraft.MainActivity.nativeDeliverSafOpenFile`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_storyteller_cadcraft_MainActivity_nativeDeliverSafOpenFile<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    file_name: JString<'caller>,
    file_bytes: JByteArray<'caller>,
) {
    let outcome = unowned_env.with_env(|env| -> Result<(), jni::errors::Error> {
        let name = file_name.to_string();
        if name.is_empty() {
            file_dialog::deliver_saf_open_contents(Vec::new());
        } else {
            let bytes = env.convert_byte_array(&file_bytes)?;
            file_dialog::deliver_saf_open_contents(vec![(name, bytes)]);
        }
        Ok(())
    });
    outcome.resolve::<ThrowRuntimeExAndDefault>();
}

/// JNI callback invoked by `MainActivity` when the user selects a destination in the Android SAF
/// `ACTION_CREATE_DOCUMENT` save dialog.
// SAFETY: Standard JNI export signature for `ai.storyteller.cadcraft.MainActivity.nativeDeliverSafSavePath`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_storyteller_cadcraft_MainActivity_nativeDeliverSafSavePath<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    save_path: JString<'caller>,
) {
    let outcome = unowned_env.with_env(|_env| -> Result<(), jni::errors::Error> {
        let path = save_path.to_string();
        file_dialog::deliver_saf_save_path(path);
        Ok(())
    });
    outcome.resolve::<ThrowRuntimeExAndDefault>();
}

/// JNI callback invoked by `MainActivity` when the user selects a folder in the Android SAF
/// `ACTION_OPEN_DOCUMENT_TREE` folder picker.
// SAFETY: Standard JNI export signature for `ai.storyteller.cadcraft.MainActivity.nativeDeliverSafFolderPath`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_storyteller_cadcraft_MainActivity_nativeDeliverSafFolderPath<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    folder_path: JString<'caller>,
) {
    let outcome = unowned_env.with_env(|_env| -> Result<(), jni::errors::Error> {
        let path = folder_path.to_string();
        file_dialog::deliver_saf_folder_path(path);
        Ok(())
    });
    outcome.resolve::<ThrowRuntimeExAndDefault>();
}
