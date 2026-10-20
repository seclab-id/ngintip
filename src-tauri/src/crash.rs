//! Crash dumps. A panic hook writes a self-contained log (message, location,
//! thread, backtrace, resident memory, environment) to `<data_dir>/crashes/` for
//! any panic on any thread, then chains to the default hook so stderr still shows
//! it. Best-effort: the hook never panics and never blocks.

use std::{
    fs,
    panic,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

static CRASH_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Directory crash dumps are written to, once [`install`] has run.
pub fn crash_dir() -> Option<PathBuf> {
    CRASH_DIR.lock().ok().and_then(|d| d.clone())
}

/// Install the panic hook and set the output directory (created if missing).
pub fn install(data_dir: &Path) {
    let dir = data_dir.join("crashes");
    let _ = fs::create_dir_all(&dir);
    if let Ok(mut slot) = CRASH_DIR.lock() {
        *slot = Some(dir);
    }
    let default = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        // Never let the dump writer itself abort the process.
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| write_dump(info)));
        default(info);
    }));
}

fn write_dump(info: &panic::PanicHookInfo<'_>) {
    let Some(dir) = crash_dir() else { return };
    let payload = info.payload();
    let message = payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic payload".into());
    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_else(|| "unknown".into());
    let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
    let backtrace = std::backtrace::Backtrace::force_capture();
    let millis = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let iso = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| millis.to_string());

    let body = format!(
        "{app} crash dump\n\
         time:       {iso}\n\
         version:    {version}\n\
         os/arch:    {os}/{arch}\n\
         thread:     {thread}\n\
         location:   {location}\n\
         memory-rss: {rss} bytes\n\
         message:    {message}\n\
         \n\
         --- backtrace ---\n{backtrace}\n",
        app = crate::brand::NAME,
        version = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        rss = crate::usage::current_rss(),
    );

    let path = dir.join(format!("crash-{millis}.log"));
    let _ = fs::write(path, body);
}

/// Summary of the crash-dump directory for the Usage panel.
pub struct CrashSummary {
    pub dir: Option<String>,
    pub count: usize,
    pub latest: Option<String>,
}

pub fn summary() -> CrashSummary {
    let Some(dir) = crash_dir() else {
        return CrashSummary { dir: None, count: 0, latest: None };
    };
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "log"))
        .collect();
    files.sort();
    let latest = files.last().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned());
    CrashSummary { dir: Some(dir.to_string_lossy().into_owned()), count: files.len(), latest }
}
