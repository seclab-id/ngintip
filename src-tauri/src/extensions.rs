//! Python extensions: user-supplied scripts that transform requests/responses on
//! the wire (the motivating case: decrypt an app's encrypted bodies so you can read
//! and edit them, then re-encrypt on the way out). A script defines
//! `process_request(req)` and/or `process_response(res)`, receiving an editable
//! HttpMessage (see runner.py); the Rust<->Python protocol stays raw bytes in/out.
//!
//! Each enabled script runs as a long-lived Python worker (see RUNNER) that speaks
//! a newline-delimited JSON protocol over stdin/stdout; stderr is captured live into
//! the debug console. A per-extension scope decides where it runs: any combination of
//! "proxy" (live history traffic), "interceptor" (messages held in the intercept
//! queue) and "repeater" (Repeater sends). The Test tab dry-runs one hook on a sample
//! message.
//!
//! Performance & robustness:
//! - `active()` is a lock-free atomic check, so traffic pays nothing when extensions
//!   are off (the default).
//! - Each worker call has a timeout, so a hung or slow script drops that worker
//!   instead of freezing the whole proxy.
//!
//! Security: the venv directory (next to the saved project by default, or a custom
//! path) derives its name from the project name, which is user-controlled, so it is
//! strictly sanitized to `[A-Za-z0-9_-]`. Every subprocess (venv creation, pip,
//! workers) is spawned with an argument ARRAY, never a shell string, so a crafted
//! project name, script path, or package cannot inject a shell command.

use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};

/// The harness that loads a user script and dispatches hooks over stdin/stdout.
const RUNNER: &str = include_str!("runner.py");

/// Most debug lines kept per session (ring buffer).
const MAX_LOG_LINES: usize = 800;

/// How long to wait for a worker's reply before giving up on it, so one slow or
/// hung script never blocks the whole proxy.
const CALL_TIMEOUT: Duration = Duration::from_secs(15);

type Logs = Arc<Mutex<VecDeque<String>>>;

fn push_log(logs: &Logs, line: impl Into<String>) {
    if let Ok(mut l) = logs.lock() {
        l.push_back(line.into());
        while l.len() > MAX_LOG_LINES {
            l.pop_front();
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Phase {
    Request,
    Response,
}

impl Phase {
    fn hook(self) -> &'static str {
        match self {
            Phase::Request => "request",
            Phase::Response => "response",
        }
    }
}

/// Where a message is being transformed: live proxy traffic, a message held in the
/// intercept queue, or a Repeater send. A given request/response is transformed at
/// exactly one of these points (an intercepted message uses `Interceptor`, not
/// `Proxy`), so an extension scoped to several surfaces never double-applies.
#[derive(Clone, Copy, PartialEq)]
pub enum Surface {
    Proxy,
    Interceptor,
    Repeater,
}

impl Surface {
    fn token(self) -> &'static str {
        match self {
            Surface::Proxy => "proxy",
            Surface::Interceptor => "interceptor",
            Surface::Repeater => "repeater",
        }
    }
}

/// Whether an extension's scope covers this surface. Scope is a comma/space separated
/// set of surface tokens ("proxy", "interceptor", "repeater"); the legacy values
/// "both" and "all" (and an empty scope) match every surface.
fn scope_matches(scope: &str, surface: Surface) -> bool {
    let s = scope.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("both") || s.eq_ignore_ascii_case("all") {
        return true;
    }
    let token = surface.token();
    s.split([',', ' ', '|', '+'])
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .any(|t| t.eq_ignore_ascii_case(token))
}

fn default_scope() -> String {
    "all".to_string()
}

/// One extension as configured by the UI (persisted per-project in the project's
/// settings, so a blank project starts with none).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionSpec {
    pub id: String,
    pub name: String,
    /// The original script path it was imported from. Used to run the file live (so
    /// external edits apply) and as a label; may not exist after the project moves.
    pub path: String,
    pub enabled: bool,
    /// Where it runs: a comma-separated set of "proxy" (live history traffic),
    /// "interceptor" (held messages) and "repeater" (Repeater sends); "all"/"both"
    /// (or empty) means every surface.
    #[serde(default = "default_scope")]
    pub scope: String,
    /// pip requirements to install into the venv (e.g. ["pycryptodome"]).
    #[serde(default)]
    pub requirements: Vec<String>,
    /// An embedded copy of the script source, so the extension travels inside the
    /// project file. Runs from `path` when that file still exists (keeping live
    /// editing), and falls back to this copy when it does not (portability). Never
    /// sent to the UI (see ExtensionState).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionsStatus {
    pub enabled: bool,
    pub python: Option<String>,
    pub venv: Option<String>,
    pub venv_ready: bool,
    pub extensions: Vec<ExtensionState>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionState {
    pub id: String,
    pub name: String,
    pub path: String,
    pub enabled: bool,
    pub scope: String,
    pub running: bool,
    pub error: Option<String>,
    pub requirements: Vec<String>,
    /// The original file is gone, so it runs from the copy saved in the project.
    pub missing: bool,
}

struct Worker {
    stdin: ChildStdin,
    /// One reply line per request, delivered by a background reader thread so calls
    /// can time out instead of blocking on a dead pipe forever.
    replies: Receiver<String>,
    child: Child,
}

struct Loaded {
    spec: ExtensionSpec,
    worker: Option<Worker>,
    error: Option<String>,
}

#[derive(Default)]
struct Inner {
    enabled: bool,
    system_python: Option<String>,
    venv_dir: Option<PathBuf>,
    venv_ready: bool,
    /// App data dir (fallback venv location + where the runner is written).
    base_dir: Option<PathBuf>,
    /// The saved project file, if any (its folder is the default venv location).
    project_file: Option<PathBuf>,
    /// A user-chosen venv directory that overrides the default when set.
    custom_venv: Option<PathBuf>,
    loaded: Vec<Loaded>,
}

#[derive(Default)]
pub struct Extensions {
    /// Lock-free fast path for the hot request/response path.
    enabled: AtomicBool,
    has_workers: AtomicBool,
    inner: Mutex<Inner>,
    logs: Logs,
}

/// Sanitize a project name into a safe directory component. The venv path is built
/// by joining this, so it must never contain a separator, `..`, or shell metachars.
pub fn sanitize_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .take(64)
        .collect();
    let trimmed = cleaned.trim_matches('_');
    if trimmed.is_empty() {
        "project".into()
    } else {
        trimmed.to_string()
    }
}

fn venv_python(venv: &Path) -> PathBuf {
    if cfg!(windows) {
        venv.join("Scripts").join("python.exe")
    } else {
        venv.join("bin").join("python")
    }
}

/// Detect a usable Python interpreter (`python3`, then `python`).
pub fn detect_python() -> Option<String> {
    for cand in ["python3", "python"] {
        if let Ok(out) = Command::new(cand).arg("--version").output() {
            if out.status.success() {
                return Some(cand.to_string());
            }
        }
    }
    None
}

impl Inner {
    fn interpreter(&self) -> Option<String> {
        if self.venv_ready {
            if let Some(v) = &self.venv_dir {
                return Some(venv_python(v).to_string_lossy().into_owned());
            }
        }
        self.system_python.clone()
    }

    fn runner_path(&self) -> Option<PathBuf> {
        self.base_dir.as_ref().map(|d| d.join("extension_runner.py"))
    }

    /// Recompute the venv directory: a custom override, else next to the saved
    /// project file, else under the app data dir.
    fn recompute_venv(&mut self) {
        let venv = if let Some(c) = self.custom_venv.clone().filter(|p| !p.as_os_str().is_empty()) {
            c
        } else if let Some(pf) = &self.project_file {
            let stem = pf.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into());
            let parent = pf.parent().map(|p| p.to_path_buf()).or_else(|| self.base_dir.clone()).unwrap_or_default();
            parent.join(format!("{}_venv_extension", sanitize_name(&stem)))
        } else {
            self.base_dir.clone().unwrap_or_default().join("scratch_venv_extension")
        };
        self.venv_ready = venv_python(&venv).exists();
        self.venv_dir = Some(venv);
    }

    fn stop_all(&mut self) {
        for l in &mut self.loaded {
            if let Some(mut w) = l.worker.take() {
                let _ = w.child.kill();
            }
        }
    }

    /// (Re)spawn workers for every enabled extension.
    fn restart_workers(&mut self, logs: &Logs) {
        self.stop_all();
        for l in &mut self.loaded {
            l.error = None;
        }
        if !self.enabled {
            return;
        }
        let Some(python) = self.interpreter() else {
            push_log(logs, "[error] no Python interpreter set; workers not started");
            return;
        };
        let Some(runner) = self.runner_path() else {
            return;
        };
        if let Err(e) = std::fs::write(&runner, RUNNER) {
            push_log(logs, format!("[error] could not write runner: {e}"));
            return;
        }
        let base_dir = self.base_dir.clone();
        let jobs: Vec<(usize, ExtensionSpec)> = self
            .loaded
            .iter()
            .enumerate()
            .filter(|(_, l)| l.spec.enabled)
            .map(|(i, l)| (i, l.spec.clone()))
            .collect();
        for (i, spec) in jobs {
            let script = match resolve_script(&spec, base_dir.as_deref()) {
                Ok(s) => s,
                Err(e) => {
                    self.loaded[i].error = Some(e.clone());
                    push_log(logs, format!("[{}] {e}", spec.name));
                    continue;
                }
            };
            match spawn_worker(&python, &runner, &script, &spec.name, logs.clone()) {
                Ok(worker) => {
                    self.loaded[i].worker = Some(worker);
                    push_log(logs, format!("[{}] started", spec.name));
                }
                Err(e) => {
                    self.loaded[i].error = Some(e.clone());
                    push_log(logs, format!("[{}] failed to start: {e}", spec.name));
                }
            }
        }
    }

    fn transform(&mut self, phase: Phase, raw: &[u8], surface: Surface, logs: &Logs) -> Option<Vec<u8>> {
        let mut cur = raw.to_vec();
        let mut changed = false;
        for i in 0..self.loaded.len() {
            if !self.loaded[i].spec.enabled
                || self.loaded[i].worker.is_none()
                || !scope_matches(&self.loaded[i].spec.scope, surface)
            {
                continue;
            }
            let name = self.loaded[i].spec.name.clone();
            let worker = self.loaded[i].worker.as_mut().unwrap();
            match worker_call(worker, phase, &cur) {
                Ok(out) => {
                    if out != cur {
                        changed = true;
                        cur = out;
                    }
                }
                Err(e) => {
                    self.loaded[i].error = Some(e.clone());
                    self.loaded[i].worker = None; // died / timed out; drop it
                    push_log(logs, format!("[{name}] {} error: {e}", phase.hook()));
                }
            }
        }
        changed.then_some(cur)
    }

    fn any_worker(&self) -> bool {
        self.loaded.iter().any(|l| l.worker.is_some())
    }
}

/// Decide which file to run for an extension. Prefer the original `path` when it
/// still exists (so editing the file on disk keeps working); otherwise materialize
/// the copy embedded in the project (`body`) to a file under the app data dir and
/// run that (so an extension travels with the project even after the file is gone).
fn resolve_script(spec: &ExtensionSpec, base_dir: Option<&Path>) -> Result<String, String> {
    if !spec.path.is_empty() && Path::new(&spec.path).exists() {
        return Ok(spec.path.clone());
    }
    if !spec.body.is_empty() {
        let base = base_dir.ok_or("no base dir to materialize the embedded script")?;
        let dir = base.join("ext_scripts");
        std::fs::create_dir_all(&dir).map_err(|e| format!("materialize dir: {e}"))?;
        let file = dir.join(format!("{}.py", sanitize_name(&spec.id)));
        std::fs::write(&file, &spec.body).map_err(|e| format!("materialize script: {e}"))?;
        return Ok(file.to_string_lossy().into_owned());
    }
    Err(format!("script not found at {} and no saved copy", spec.path))
}

/// Spawn a worker process with background reader threads for stdout (replies) and
/// stderr (debug output).
fn spawn_worker(python: &str, runner: &Path, script: &str, name: &str, logs: Logs) -> Result<Worker, String> {
    let mut child = Command::new(python)
        .arg(runner)
        .arg(script) // argument array: the path is data, never shell-interpreted
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn failed: {e}"))?;

    let stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let stderr = child.stderr.take();

    let (tx, replies) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    if let Some(err) = stderr {
        let name = name.to_string();
        thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                push_log(&logs, format!("[{name}] {line}"));
            }
        });
    }
    Ok(Worker { stdin, replies, child })
}

/// Result of a Test-tab dry run.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub output: String,
    pub changed: bool,
    pub logs: Vec<String>,
    pub error: Option<String>,
}

/// Run the script once on `raw` in a throwaway process (stdin closed after one line,
/// so it exits), with a timeout. Returns (transformed bytes, printed lines).
fn run_once(python: &str, runner: &Path, script: &str, phase: Phase, raw: &[u8]) -> Result<(Vec<u8>, Vec<String>), String> {
    let mut child = Command::new(python)
        .arg(runner)
        .arg(script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn failed: {e}"))?;

    let line = serde_json::to_string(&Msg { hook: phase.hook(), data: STANDARD.encode(raw) }).map_err(|e| e.to_string())?;
    {
        let mut stdin = child.stdin.take().ok_or("no stdin")?;
        stdin.write_all(line.as_bytes()).map_err(|e| format!("write: {e}"))?;
        stdin.write_all(b"\n").map_err(|e| format!("write: {e}"))?;
    } // stdin dropped -> EOF, so the runner exits after this one message

    let stdout = child.stdout.take().ok_or("no stdout")?;
    let stderr = child.stderr.take().ok_or("no stderr")?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut s = String::new();
        let _ = BufReader::new(stdout).read_line(&mut s);
        let _ = tx.send(s);
    });
    let err_thread = thread::spawn(move || BufReader::new(stderr).lines().map_while(Result::ok).collect::<Vec<String>>());

    let reply = rx.recv_timeout(CALL_TIMEOUT);
    let _ = child.kill();
    let _ = child.wait();
    let logs = err_thread.join().unwrap_or_default();

    match reply {
        Ok(line) if !line.trim().is_empty() => {
            let r: Reply = serde_json::from_str(line.trim()).map_err(|e| format!("bad reply: {e}"))?;
            if !r.ok {
                return Err(r.error.unwrap_or_else(|| "script error".into()));
            }
            let out = STANDARD.decode(r.data.trim()).map_err(|e| format!("bad base64: {e}"))?;
            Ok((out, logs))
        }
        Ok(_) => Err(format!("no reply from script{}", if logs.is_empty() { String::new() } else { format!(":\n{}", logs.join("\n")) })),
        Err(_) => Err(format!("timed out after {}s", CALL_TIMEOUT.as_secs())),
    }
}

#[derive(Serialize)]
struct Msg<'a> {
    hook: &'a str,
    data: String,
}

#[derive(Deserialize)]
struct Reply {
    ok: bool,
    #[serde(default)]
    data: String,
    #[serde(default)]
    error: Option<String>,
}

/// One request/response over a worker's pipes, with a timeout.
fn worker_call(w: &mut Worker, phase: Phase, raw: &[u8]) -> Result<Vec<u8>, String> {
    let msg = Msg { hook: phase.hook(), data: STANDARD.encode(raw) };
    let line = serde_json::to_string(&msg).map_err(|e| e.to_string())?;
    w.stdin.write_all(line.as_bytes()).map_err(|e| format!("write: {e}"))?;
    w.stdin.write_all(b"\n").map_err(|e| format!("write: {e}"))?;
    w.stdin.flush().map_err(|e| format!("flush: {e}"))?;

    let resp = match w.replies.recv_timeout(CALL_TIMEOUT) {
        Ok(line) => line,
        Err(RecvTimeoutError::Timeout) => return Err(format!("timed out after {}s", CALL_TIMEOUT.as_secs())),
        Err(RecvTimeoutError::Disconnected) => return Err("worker exited".into()),
    };
    let reply: Reply = serde_json::from_str(resp.trim()).map_err(|e| format!("bad reply: {e}"))?;
    if !reply.ok {
        return Err(reply.error.unwrap_or_else(|| "script error".into()));
    }
    STANDARD.decode(reply.data.trim()).map_err(|e| format!("bad base64: {e}"))
}

impl Extensions {
    /// Recompute the lock-free flags after any change to enabled/workers.
    fn refresh(&self, inner: &Inner) {
        self.enabled.store(inner.enabled, Ordering::Relaxed);
        self.has_workers.store(inner.any_worker(), Ordering::Relaxed);
    }

    /// Point the manager at the app data dir, the open project file (whose folder is
    /// the default venv location), and an optional custom venv override. Restarts
    /// workers only when the effective interpreter actually changes.
    pub fn configure(&self, base_dir: PathBuf, project_file: Option<PathBuf>, custom_venv: Option<PathBuf>) {
        let mut inner = self.inner.lock().unwrap();
        let before = inner.interpreter();
        inner.base_dir = Some(base_dir);
        inner.project_file = project_file;
        inner.custom_venv = custom_venv.filter(|p| !p.as_os_str().is_empty());
        inner.recompute_venv();
        if inner.system_python.is_none() {
            inner.system_python = detect_python();
        }
        if inner.interpreter() != before {
            inner.restart_workers(&self.logs);
        }
        self.refresh(&inner);
    }

    /// Set (or clear) the custom venv directory and recompute where the venv lives.
    pub fn set_venv_path(&self, custom: Option<PathBuf>) {
        let mut inner = self.inner.lock().unwrap();
        let before = inner.interpreter();
        inner.custom_venv = custom.filter(|p| !p.as_os_str().is_empty());
        inner.recompute_venv();
        if inner.interpreter() != before {
            inner.restart_workers(&self.logs);
        }
        self.refresh(&inner);
    }

    pub fn set_python(&self, python: Option<String>) {
        let mut inner = self.inner.lock().unwrap();
        let before = inner.interpreter();
        inner.system_python = python.filter(|s| !s.trim().is_empty()).or_else(detect_python);
        // Only respawn when the effective interpreter actually changes (so re-applying
        // the same per-project interpreter, e.g. on every save, is a no-op).
        if inner.interpreter() != before {
            inner.restart_workers(&self.logs);
        }
        self.refresh(&inner);
    }

    pub fn set_enabled(&self, on: bool) {
        let mut inner = self.inner.lock().unwrap();
        inner.enabled = on;
        inner.restart_workers(&self.logs);
        self.refresh(&inner);
    }

    /// Enable/disable a single loaded extension by id. Returns false if not found.
    pub fn set_extension_enabled(&self, id: &str, on: bool) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let found = match inner.loaded.iter_mut().find(|l| l.spec.id == id) {
            Some(l) => {
                l.spec.enabled = on;
                true
            }
            None => false,
        };
        if found {
            inner.restart_workers(&self.logs);
            self.refresh(&inner);
        }
        found
    }

    pub fn set_extensions(&self, specs: Vec<ExtensionSpec>) {
        let mut inner = self.inner.lock().unwrap();
        inner.stop_all();
        inner.loaded = specs.into_iter().map(|spec| Loaded { spec, worker: None, error: None }).collect();
        inner.restart_workers(&self.logs);
        self.refresh(&inner);
    }

    /// Replace the whole extension set, the on/off state AND the saved debug console in
    /// one step (one worker restart), used when loading a project's extensions. The
    /// saved console is restored first, so the fresh worker-start lines append after it.
    pub fn load(&self, specs: Vec<ExtensionSpec>, enabled: bool, saved_logs: Vec<String>) {
        let mut inner = self.inner.lock().unwrap();
        inner.stop_all();
        inner.loaded = specs.into_iter().map(|spec| Loaded { spec, worker: None, error: None }).collect();
        inner.enabled = enabled;
        if let Ok(mut l) = self.logs.lock() {
            l.clear();
            let skip = saved_logs.len().saturating_sub(MAX_LOG_LINES);
            for line in saved_logs.into_iter().skip(skip) {
                l.push_back(line);
            }
        }
        inner.restart_workers(&self.logs);
        self.refresh(&inner);
    }

    /// Create the venv (if missing) and upgrade pip. Returns (interpreter, venv path).
    /// Locks only briefly; the pip work runs unlocked so status/active stay responsive.
    fn ensure_venv(&self) -> Result<(String, PathBuf), String> {
        let (python, venv) = {
            let inner = self.inner.lock().unwrap();
            let python = inner.system_python.clone().ok_or("No Python interpreter. Set or detect one first.")?;
            let venv = inner.venv_dir.clone().ok_or("No venv path set.")?;
            (python, venv)
        };
        if !venv_python(&venv).exists() {
            let create = Command::new(&python)
                .args(["-m", "venv"])
                .arg(&venv)
                .output()
                .map_err(|e| format!("venv create failed: {e}"))?;
            if !create.status.success() {
                return Err(format!("venv creation failed:\n{}", String::from_utf8_lossy(&create.stderr)));
            }
            let _ = Command::new(venv_python(&venv)).args(["-m", "pip", "install", "--upgrade", "pip"]).output();
            let mut inner = self.inner.lock().unwrap();
            inner.venv_ready = true;
            push_log(&self.logs, format!("[venv] created at {}", venv.display()));
            self.refresh(&inner);
        }
        Ok((python, venv))
    }

    /// Create the venv and pip-install the given packages (blocking). With no packages
    /// it just ensures the venv exists. Returns the pip log.
    pub fn install_packages(&self, packages: Vec<String>) -> Result<String, String> {
        let (_python, venv) = self.ensure_venv()?;
        let vpy = venv_python(&venv);
        let pkgs: Vec<String> = packages.into_iter().map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect();
        let mut out = format!("venv: {}\n", venv.display());
        if !pkgs.is_empty() {
            let mut args = vec!["-m".to_string(), "pip".to_string(), "install".to_string()];
            args.extend(pkgs.iter().cloned());
            let pip = Command::new(&vpy).args(&args).output().map_err(|e| format!("pip failed: {e}"))?;
            out.push_str(&format!("$ pip install {}\n", pkgs.join(" ")));
            out.push_str(&String::from_utf8_lossy(&pip.stdout));
            out.push_str(&String::from_utf8_lossy(&pip.stderr));
            if !pip.status.success() {
                return Err(out);
            }
            let inner = self.inner.lock().unwrap();
            self.refresh(&inner);
            drop(inner);
            self.inner.lock().unwrap().restart_workers(&self.logs);
        }
        Ok(out)
    }

    /// Create the venv and install every loaded extension's declared requirements.
    pub fn setup_venv(&self) -> Result<String, String> {
        let reqs: Vec<String> = {
            let inner = self.inner.lock().unwrap();
            inner.loaded.iter().flat_map(|l| l.spec.requirements.iter().cloned()).collect()
        };
        self.install_packages(reqs)
    }

    /// Packages installed in the venv (`pip freeze`), or empty when it isn't built.
    pub fn installed_packages(&self) -> Vec<String> {
        let venv = match self.inner.lock().unwrap().venv_dir.clone() {
            Some(v) => v,
            None => return Vec::new(),
        };
        let vpy = venv_python(&venv);
        if !vpy.exists() {
            return Vec::new();
        }
        match Command::new(&vpy).args(["-m", "pip", "freeze"]).output() {
            Ok(o) => String::from_utf8_lossy(&o.stdout).lines().map(str::to_string).filter(|l| !l.is_empty()).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// True when an in-scope message should be run through extensions. Lock-free, so
    /// the request/response hot path pays nothing when extensions are off.
    pub fn active(&self) -> bool {
        self.enabled.load(Ordering::Relaxed) && self.has_workers.load(Ordering::Relaxed)
    }

    /// Transform a message; returns the new bytes only when something changed.
    pub fn transform(&self, phase: Phase, raw: &[u8], surface: Surface) -> Option<Vec<u8>> {
        let mut inner = self.inner.lock().unwrap();
        let result = inner.transform(phase, raw, surface, &self.logs);
        // A worker may have died / timed out mid-call; keep the fast-path flag honest.
        let any = inner.any_worker();
        drop(inner);
        self.has_workers.store(any, Ordering::Relaxed);
        result
    }

    pub fn status(&self) -> ExtensionsStatus {
        let inner = self.inner.lock().unwrap();
        ExtensionsStatus {
            enabled: inner.enabled,
            python: inner.system_python.clone(),
            venv: inner.venv_dir.as_ref().map(|v| v.to_string_lossy().into_owned()),
            venv_ready: inner.venv_ready,
            extensions: inner
                .loaded
                .iter()
                .map(|l| ExtensionState {
                    id: l.spec.id.clone(),
                    name: l.spec.name.clone(),
                    path: l.spec.path.clone(),
                    enabled: l.spec.enabled,
                    scope: l.spec.scope.clone(),
                    running: l.worker.is_some(),
                    error: l.error.clone(),
                    requirements: l.spec.requirements.clone(),
                    missing: !l.spec.path.is_empty() && !Path::new(&l.spec.path).exists() && !l.spec.body.is_empty(),
                })
                .collect(),
        }
    }

    /// Run one script's hook once on the given input, in a throwaway process, and
    /// return the transformed message plus whatever it printed. Used by the Test tab;
    /// it uses the venv interpreter so imports (e.g. pycryptodome) work.
    pub fn test(&self, id: &str, phase: Phase, raw: &[u8]) -> TestResult {
        let (python, runner, script) = {
            let inner = self.inner.lock().unwrap();
            let Some(l) = inner.loaded.iter().find(|l| l.spec.id == id) else {
                return TestResult { error: Some(format!("no extension with id {id}")), ..Default::default() };
            };
            let script = match resolve_script(&l.spec, inner.base_dir.as_deref()) {
                Ok(s) => s,
                Err(e) => return TestResult { error: Some(e), ..Default::default() },
            };
            let Some(python) = inner.interpreter() else {
                return TestResult { error: Some("No Python interpreter set.".into()), ..Default::default() };
            };
            let Some(runner) = inner.runner_path() else {
                return TestResult { error: Some("No project open.".into()), ..Default::default() };
            };
            (python, runner, script)
        };
        if let Err(e) = std::fs::write(&runner, RUNNER) {
            return TestResult { error: Some(format!("could not write runner: {e}")), ..Default::default() };
        }
        match run_once(&python, &runner, &script, phase, raw) {
            Ok((out, logs)) => TestResult {
                changed: out != raw,
                output: String::from_utf8_lossy(&out).into_owned(),
                logs,
                error: None,
            },
            Err(e) => TestResult { error: Some(e), ..Default::default() },
        }
    }

    pub fn logs(&self) -> Vec<String> {
        self.logs.lock().map(|l| l.iter().cloned().collect()).unwrap_or_default()
    }

    pub fn clear_logs(&self) {
        if let Ok(mut l) = self.logs.lock() {
            l.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_user_project_names_for_the_venv() {
        assert_eq!(sanitize_name("simbka"), "simbka");
        assert_eq!(sanitize_name("../../etc/passwd"), "etc_passwd");
        assert_eq!(sanitize_name("a b; rm -rf /"), "a_b__rm_-rf");
        assert_eq!(sanitize_name("$(whoami)`id`"), "whoami__id");
        assert_eq!(sanitize_name(""), "project");
        assert_eq!(sanitize_name("///"), "project");
        for name in ["../x", "a/b", "..", "a\\b"] {
            let s = sanitize_name(name);
            assert!(!s.contains('/') && !s.contains('\\') && s != "..", "leaked: {s}");
        }
    }

    #[test]
    fn venv_python_path_is_platform_correct() {
        let v = PathBuf::from("/tmp/x_venv_extension");
        let p = venv_python(&v);
        if cfg!(windows) {
            assert!(p.ends_with("Scripts/python.exe") || p.ends_with("Scripts\\python.exe"));
        } else {
            assert!(p.ends_with("bin/python"));
        }
    }

    #[test]
    fn scope_matches_covers_the_three_surfaces() {
        use Surface::*;
        // Legacy / catch-all values match every surface.
        for scope in ["all", "both", "", "  ", "ALL"] {
            assert!(scope_matches(scope, Proxy), "{scope:?} proxy");
            assert!(scope_matches(scope, Interceptor), "{scope:?} interceptor");
            assert!(scope_matches(scope, Repeater), "{scope:?} repeater");
        }
        // A single surface matches only itself.
        assert!(scope_matches("proxy", Proxy));
        assert!(!scope_matches("proxy", Interceptor));
        assert!(!scope_matches("proxy", Repeater));
        assert!(scope_matches("interceptor", Interceptor));
        assert!(!scope_matches("interceptor", Proxy));
        assert!(scope_matches("repeater", Repeater));
        assert!(!scope_matches("repeater", Proxy));
        // Combinations (comma / space / mixed case) match each listed surface.
        assert!(scope_matches("proxy,repeater", Proxy));
        assert!(scope_matches("proxy,repeater", Repeater));
        assert!(!scope_matches("proxy,repeater", Interceptor));
        assert!(scope_matches("Interceptor Repeater", Interceptor));
        assert!(scope_matches("Interceptor Repeater", Repeater));
        assert!(!scope_matches("interceptor,repeater", Proxy));
    }

    fn worker(script: &str) -> (Worker, PathBuf) {
        let python = detect_python().expect("python");
        let dir = std::env::temp_dir().join(format!("ext-test-{}-{}", std::process::id(), rand_suffix()));
        std::fs::create_dir_all(&dir).unwrap();
        let runner = dir.join("runner.py");
        std::fs::write(&runner, RUNNER).unwrap();
        let s = dir.join("s.py");
        std::fs::write(&s, script).unwrap();
        let logs: Logs = Default::default();
        (spawn_worker(&python, &runner, &s.to_string_lossy(), "t", logs).unwrap(), dir)
    }
    fn rand_suffix() -> u128 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    }

    #[test]
    fn worker_transforms_with_real_python() {
        if detect_python().is_none() {
            eprintln!("skipping: no python");
            return;
        }
        // The hook receives an HttpMessage from the new API and edits it in place.
        let (mut w, dir) = worker("def process_request(req):\n    req.set_header('X-Test', '1')\n    req.method = 'PUT'\n");
        let out = worker_call(&mut w, Phase::Request, b"GET /a HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let out = String::from_utf8_lossy(&out);
        assert!(out.starts_with("PUT /a HTTP/1.1"), "method edited: {out}");
        assert!(out.contains("X-Test: 1"), "header added: {out}");
        // No response hook -> passthrough unchanged.
        let res = b"HTTP/1.1 200 OK\r\n\r\n";
        assert_eq!(worker_call(&mut w, Phase::Response, res).unwrap(), res);
        let _ = w.child.kill();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_dry_run_reports_output_and_logs() {
        if detect_python().is_none() {
            eprintln!("skipping: no python");
            return;
        }
        let dir = std::env::temp_dir().join(format!("ext-testrun-{}-{}", std::process::id(), rand_suffix()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("t.py");
        std::fs::write(&script, "def process_request(req):\n    print('hello from test')\n    req.set_header('X-T', '1')\n").unwrap();
        let ext = Extensions::default();
        ext.configure(dir.clone(), None, None);
        ext.set_extensions(vec![ExtensionSpec {
            id: "t".into(),
            name: "t".into(),
            path: script.to_string_lossy().into_owned(),
            enabled: false,
            scope: "both".into(),
            requirements: vec![],
            body: String::new(),
        }]);
        let res = ext.test("t", Phase::Request, b"GET /a HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(res.error.is_none(), "error: {:?}", res.error);
        assert!(res.changed, "output should differ");
        assert!(res.output.contains("X-T: 1"), "output: {}", res.output);
        assert!(res.logs.iter().any(|l| l.contains("hello from test")), "logs: {:?}", res.logs);
        assert!(ext.test("nope", Phase::Request, b"x").error.is_some(), "unknown id errors");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_script_prefers_live_file_then_embedded_body() {
        let dir = std::env::temp_dir().join(format!("ext-resolve-{}-{}", std::process::id(), rand_suffix()));
        std::fs::create_dir_all(&dir).unwrap();
        let live = dir.join("live.py");
        std::fs::write(&live, "def process_request(req):\n    pass\n").unwrap();
        let base = dir.join("appdata");
        std::fs::create_dir_all(&base).unwrap();

        // 1) When the original file exists, it is used as-is (live editing).
        let spec = ExtensionSpec {
            id: "e1".into(),
            name: "e1".into(),
            path: live.to_string_lossy().into_owned(),
            enabled: true,
            scope: "all".into(),
            requirements: vec![],
            body: "print('embedded')".into(),
        };
        assert_eq!(resolve_script(&spec, Some(&base)).unwrap(), live.to_string_lossy());

        // 2) When the file is gone, the embedded copy is materialized and run.
        let gone = ExtensionSpec {
            id: "e2".into(),
            path: dir.join("missing.py").to_string_lossy().into_owned(),
            body: "print('embedded')".into(),
            ..spec.clone()
        };
        let out = resolve_script(&gone, Some(&base)).unwrap();
        assert!(out.ends_with("ext_scripts/e2.py") || out.ends_with("ext_scripts\\e2.py"), "materialized path: {out}");
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "print('embedded')");

        // 3) No file and no body is an error, not a silent pass.
        let empty = ExtensionSpec { id: "e3".into(), path: String::new(), body: String::new(), ..spec.clone() };
        assert!(resolve_script(&empty, Some(&base)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn slow_worker_times_out_instead_of_hanging() {
        if detect_python().is_none() {
            eprintln!("skipping: no python");
            return;
        }
        // A script that never replies would hang without the timeout; use a tiny one
        // by shrinking the timeout via a hung hook and asserting we recover.
        let (mut w, dir) = worker("import time\ndef process_request(raw):\n    time.sleep(60)\n    return raw\n");
        // Manually drive a call with a short deadline by racing the reply channel.
        let msg = super::Msg { hook: "request", data: STANDARD.encode(b"x") };
        let line = serde_json::to_string(&msg).unwrap();
        w.stdin.write_all(line.as_bytes()).unwrap();
        w.stdin.write_all(b"\n").unwrap();
        w.stdin.flush().unwrap();
        let got = w.replies.recv_timeout(Duration::from_millis(300));
        assert!(matches!(got, Err(RecvTimeoutError::Timeout)), "expected timeout, got {got:?}");
        let _ = w.child.kill();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
