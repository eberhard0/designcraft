//! DesignCraft on Android.
//!
//! Runs the same [`designcraft_ui_egui::DesignApp`] as the desktop app inside a `GameActivity`
//! (android-activity's `game-activity` backend, which eframe needs for the soft keyboard and
//! accesskit). Built with `cargo ndk` into `android/app/src/main/jniLibs`, then packaged by the
//! Gradle project in `android/`.
//!
//! Differences from the desktop app (the services mirror the web shell):
//! - no TCP control channel;
//! - File › Open / Place / Load Swatches ask `MainActivity.pickOpen()` (Storage Access
//!   Framework); the bytes come back on a Java thread through `nativeDeliverFile` into
//!   `Services::inbox` (`.designcraft`/`.idml` → `file.openBytes`, `.ase` → `swatch.load`,
//!   anything else → `file.place`);
//! - Save and Export write to `Downloads/DesignCraft/<name>` through
//!   `MainActivity.saveToDownloads` (MediaStore, no dialog); saving the same name again in one
//!   session overwrites that file;
//! - Help links open through `MainActivity.openUrl`;
//! - UI state, engine preferences and crash recovery live in the app's private files directory.

#![cfg(target_os = "android")]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

use android_activity::AndroidApp;
use designcraft_engine::Session;
use designcraft_ui_egui::{DesignApp, Inbox, Services};
use jni::objects::{JByteArray, JObject, JString};
use jni::{Env, EnvUnowned, JavaVM, jni_sig, jni_str};

const LOG_TAG: &str = "designcraft";
/// How often unsaved preference changes are written (Android may end the process without `on_exit`).
const PREFS_INTERVAL_SECS: f64 = 20.0;

/// Files the Kotlin side delivers (name, bytes); the app opens them on its next frame.
static INBOX: OnceLock<Inbox> = OnceLock::new();
/// The egui context, to wake the app when a file arrives from a Java thread.
static CTX: OnceLock<egui::Context> = OnceLock::new();
/// The process's Java VM (set once) and the current activity (a reference android-activity
/// owns, stored as an address; 0 = none).
static VM: OnceLock<JavaVM> = OnceLock::new();
static ACTIVITY: Mutex<usize> = Mutex::new(0);

fn inbox() -> &'static Inbox {
    INBOX.get_or_init(Inbox::default)
}

/// The activity's entry point, called by android-activity's GameActivity glue on its own thread.
/// It returns when the activity is destroyed.
#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    static LOGGER: OnceLock<()> = OnceLock::new();
    LOGGER.get_or_init(|| {
        android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info).with_tag(LOG_TAG));
    });
    // SAFETY: `vm_as_ptr` is the process's JavaVM, valid for the life of the process.
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) };
    let _ = VM.set(vm);
    *ACTIVITY.lock().unwrap_or_else(PoisonError::into_inner) = app.activity_as_ptr() as usize;

    let data_dir = app.internal_data_path().unwrap_or_else(|| PathBuf::from("/data/local/tmp"));
    log::info!("DesignCraft {} starting; data in {}", env!("CARGO_PKG_VERSION"), data_dir.display());
    let options = eframe::NativeOptions {
        android_app: Some(app),
        // eframe saves egui panel/window sizes here on exit.
        persistence_path: Some(data_dir.join("ui.ron")),
        ..Default::default()
    };
    let result = eframe::run_native(
        "DesignCraft",
        options,
        Box::new(move |cc| {
            let _ = CTX.set(cc.egui_ctx.clone());
            if let Some(rs) = &cc.wgpu_render_state {
                let info = rs.adapter.get_info();
                log::info!("wgpu backend {:?}, adapter {}", info.backend, info.name);
            }
            let mut session = Session::new();
            // Crash recovery: reopen what a previous run left unsaved, then keep it current.
            session.recovery_dir = Some(data_dir.join("recovery"));
            let recovered = session.execute("file.recovery.open", &serde_json::json!({})).ok();
            let mut app = DesignApp::new(session, services());
            if let Some(n) = recovered.as_ref().and_then(|r| r["opened"].as_array()).map(Vec::len).filter(|n| *n > 0) {
                app.status(format!("Recovered {n} unsaved document{} from the last session.", if n == 1 { "" } else { "s" }));
            }
            let mut shell = AndroidShell { app, prefs_dir: data_dir.clone(), last_saved: None, last_prefs_time: 0.0 };
            shell.load_prefs();
            Ok(Box::new(shell))
        }),
    );
    *ACTIVITY.lock().unwrap_or_else(PoisonError::into_inner) = 0;
    if let Err(e) = result {
        log::error!("DesignCraft stopped: {e}");
        // winit allows one event loop per process: when Android recreates the activity in the
        // same process, end the process so the next launch starts clean instead of a blank window.
        std::process::exit(0);
    }
}

/// Wraps the app like the desktop shell does: preferences on disk, and Help links routed to the
/// system browser (eframe's `links` feature has no Android backend).
struct AndroidShell {
    app: DesignApp,
    prefs_dir: PathBuf,
    /// The last (ui.json, prefs.json) written, to skip unchanged saves.
    last_saved: Option<(Vec<u8>, Vec<u8>)>,
    last_prefs_time: f64,
}

impl AndroidShell {
    fn load_prefs(&mut self) {
        if let Ok(bytes) = std::fs::read(self.prefs_dir.join("ui.json"))
            && let Ok(ui) = serde_json::from_slice::<designcraft_ui_egui::UiState>(&bytes)
        {
            self.app.ui = ui;
        }
        // Engine preferences (Preferences dialog, favourites…) live beside the UI state.
        if let Ok(bytes) = std::fs::read(self.prefs_dir.join("prefs.json"))
            && let Ok(prefs) = serde_json::from_slice::<designcraft_engine::Prefs>(&bytes)
        {
            self.app.session.prefs = prefs;
        }
    }

    fn save_prefs(&mut self) {
        let (Ok(ui), Ok(prefs)) = (serde_json::to_vec_pretty(&self.app.ui), serde_json::to_vec_pretty(&self.app.session.prefs)) else {
            return;
        };
        if self.last_saved.as_ref().is_some_and(|(u, p)| *u == ui && *p == prefs) {
            return;
        }
        if let Err(e) = write_atomic(&self.prefs_dir.join("ui.json"), &ui) {
            log::warn!("couldn't save ui.json: {e}");
        }
        if let Err(e) = write_atomic(&self.prefs_dir.join("prefs.json"), &prefs) {
            log::warn!("couldn't save prefs.json: {e}");
        }
        self.last_saved = Some((ui, prefs));
    }
}

impl eframe::App for AndroidShell {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.app.logic(ctx);
        let now = ctx.input(|i| i.time);
        if now - self.last_prefs_time > PREFS_INTERVAL_SECS {
            self.last_prefs_time = now;
            self.save_prefs();
        }
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.app.raw_input_hook(raw);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.app.ui(ui);
        // Help menu links: egui queues them as output commands; hand them to the activity.
        let urls: Vec<String> = ui.ctx().output_mut(|o| {
            let mut urls = Vec::new();
            o.commands.retain(|c| match c {
                egui::OutputCommand::OpenUrl(u) => {
                    urls.push(u.url.clone());
                    false
                }
                _ => true,
            });
            urls
        });
        for url in urls {
            if let Err(e) = open_url(&url) {
                log::error!("couldn't open {url}: {e}");
            }
        }
    }

    fn on_exit(&mut self) {
        self.save_prefs();
    }
}

/// The web shell's services with Android implementations: asynchronous open through the system
/// picker (the file arrives through the inbox) and saving into Downloads/DesignCraft.
fn services() -> Services {
    Services {
        open_async: Some(Box::new(|purpose: &str| {
            if let Err(e) = pick_open(purpose) {
                log::error!("couldn't open the file picker: {e}");
            }
        })),
        // Exports ask for a name; the suggested name becomes the file name in Downloads/DesignCraft.
        pick_save: Some(Box::new(|name: &str| Some(file_name(name)))),
        read: Some(Box::new(|p: &str| std::fs::read(p).map_err(|e| e.to_string()))),
        write: Some(Box::new(|path: &str, bytes: &[u8]| save_to_downloads(&file_name(path), bytes))),
        download: Some(Box::new(|name: &str, bytes: &[u8]| {
            if let Err(e) = save_to_downloads(&file_name(name), bytes) {
                log::error!("saving {name} failed: {e}");
            }
        })),
        inbox: Some(inbox().clone()),
        ..Default::default()
    }
}

fn file_name(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string())
}

/// Write `bytes` to `path` through a temporary file, so a crash mid-write keeps the old file.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

// ---- Calls into MainActivity (Kotlin) ----------------------------------------------------------

/// Run `f` with a JNI environment on this thread and the current activity.
fn with_activity<T>(f: impl FnOnce(&mut Env<'_>, &JObject<'_>) -> jni::errors::Result<T>) -> Result<T, String> {
    let vm = VM.get().ok_or("the Java VM is not available")?;
    let raw = *ACTIVITY.lock().unwrap_or_else(PoisonError::into_inner);
    if raw == 0 {
        return Err("the activity is not running".to_string());
    }
    let raw = raw as jni::sys::jobject;
    vm.attach_current_thread(|env| -> jni::errors::Result<T> {
        // SAFETY: the reference comes from android-activity's `activity_as_ptr`, which keeps it
        // valid while the activity runs (ACTIVITY is cleared when `run_native` returns). `Cast`
        // neither owns nor deletes it.
        let activity = unsafe { env.as_cast_raw::<JObject>(&raw)? };
        f(env, &activity)
    })
    .map_err(|e| e.to_string())
}

/// `MainActivity.pickOpen(purpose)`: show the system file picker; the result comes through the inbox.
fn pick_open(purpose: &str) -> Result<(), String> {
    with_activity(|env, activity| {
        let jpurpose = JString::from_str(env, purpose)?;
        env.call_method(activity, jni_str!("pickOpen"), jni_sig!("(Ljava/lang/String;)V"), &[(&jpurpose).into()])?;
        Ok(())
    })
}

/// `MainActivity.saveToDownloads(name, bytes)`: `null` on success, else the error message.
fn save_to_downloads(name: &str, bytes: &[u8]) -> Result<(), String> {
    with_activity(|env, activity| {
        let jname = JString::from_str(env, name)?;
        let jbytes = env.byte_array_from_slice(bytes)?;
        let ret = env
            .call_method(activity, jni_str!("saveToDownloads"), jni_sig!("(Ljava/lang/String;[B)Ljava/lang/String;"), &[(&jname).into(), (&jbytes).into()])?
            .l()?;
        if ret.is_null() {
            return Ok(Ok(()));
        }
        let message = env.cast_local::<JString>(ret)?;
        Ok(Err(message.to_string()))
    })?
}

/// `MainActivity.openUrl(url)`: Help menu links in the browser.
fn open_url(url: &str) -> Result<(), String> {
    with_activity(|env, activity| {
        let jurl = JString::from_str(env, url)?;
        env.call_method(activity, jni_str!("openUrl"), jni_sig!("(Ljava/lang/String;)V"), &[(&jurl).into()])?;
        Ok(())
    })
}

// ---- Calls from MainActivity (Kotlin) ----------------------------------------------------------

/// `MainActivity.nativeDeliverFile(name, bytes)`: a picked file's contents, from a Java thread.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_iameberhard_designcraft_MainActivity_nativeDeliverFile<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    name: JString<'caller>,
    bytes: JByteArray<'caller>,
) {
    let outcome = unowned_env.with_env(|env| -> jni::errors::Result<()> {
        let name = name.to_string();
        let bytes = env.convert_byte_array(&bytes)?;
        log::info!("received {name} ({} bytes)", bytes.len());
        inbox().lock().unwrap_or_else(PoisonError::into_inner).push((name, bytes));
        if let Some(ctx) = CTX.get() {
            ctx.request_repaint();
        }
        Ok(())
    });
    outcome.resolve::<jni::errors::LogErrorAndDefault>()
}
