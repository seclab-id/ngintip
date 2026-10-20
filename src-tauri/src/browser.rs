//! Browser engine: find, download and launch a Chromium/Chrome binary wired to
//! the proxy so a pentester can browse and have everything recorded.
//!
//! Downloads use Google's "Chrome for Testing" (CfT) which ships a plain .zip on
//! every desktop platform; extraction uses the OS tool (ditto/unzip/Expand-Archive)
//! so app bundles, symlinks and executable bits survive.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// CfT index of the latest known-good versions with per-platform download URLs.
const CFT_JSON: &str = "https://googlechromefortesting.storage.googleapis.com/last-known-good-versions-with-downloads.json";

/// Standard install locations to probe for an already-installed browser.
pub fn candidates() -> Vec<PathBuf> {
    let p = |s: &str| PathBuf::from(s);
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_default();
        let user = |s: &str| PathBuf::from(format!("{home}{s}"));
        vec![
            p("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
            user("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
            p("/Applications/Chromium.app/Contents/MacOS/Chromium"),
            p("/Applications/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing"),
            p("/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge"),
            p("/Applications/Brave Browser.app/Contents/MacOS/Brave Browser"),
        ]
    }
    #[cfg(target_os = "linux")]
    {
        vec![
            p("/usr/bin/google-chrome"),
            p("/usr/bin/google-chrome-stable"),
            p("/usr/bin/chromium"),
            p("/usr/bin/chromium-browser"),
            p("/snap/bin/chromium"),
            p("/usr/bin/microsoft-edge"),
            p("/usr/bin/brave-browser"),
        ]
    }
    #[cfg(target_os = "windows")]
    {
        let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".into());
        let pf86 = std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| "C:\\Program Files (x86)".into());
        let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
        vec![
            p(&format!("{pf}\\Google\\Chrome\\Application\\chrome.exe")),
            p(&format!("{pf86}\\Google\\Chrome\\Application\\chrome.exe")),
            p(&format!("{local}\\Google\\Chrome\\Application\\chrome.exe")),
            p(&format!("{pf}\\Microsoft\\Edge\\Application\\msedge.exe")),
            p(&format!("{pf86}\\Microsoft\\Edge\\Application\\msedge.exe")),
        ]
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = p;
        vec![]
    }
}

/// First installed browser found, as a path string.
pub fn detect() -> Option<String> {
    candidates().into_iter().find(|c| c.exists()).map(|c| c.to_string_lossy().into_owned())
}

/// CfT platform key for this OS/arch, or None when auto-download is unsupported.
pub fn cft_platform() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("mac-arm64"),
        ("macos", _) => Some("mac-x64"),
        ("linux", _) => Some("linux64"),
        ("windows", _) => Some("win64"),
        _ => None,
    }
}

/// The Stable "chrome" download URL for `platform` from a CfT index document.
pub fn pick_download_url(json: &Value, platform: &str) -> Option<String> {
    json.get("channels")?
        .get("Stable")?
        .get("downloads")?
        .get("chrome")?
        .as_array()?
        .iter()
        .find(|d| d.get("platform").and_then(|p| p.as_str()) == Some(platform))
        .and_then(|d| d.get("url").and_then(|u| u.as_str()))
        .map(|s| s.to_string())
}

/// The browser binary inside a folder extracted from a CfT zip for `platform`.
pub fn extracted_binary(dir: &Path, platform: &str) -> Option<PathBuf> {
    let sub = dir.join(format!("chrome-{platform}"));
    let cand = match platform {
        "linux64" => sub.join("chrome"),
        "win64" => sub.join("chrome.exe"),
        _ => sub.join("Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing"),
    };
    cand.exists().then_some(cand)
}

/// If `path` is a macOS `.app` bundle, resolve it to the executable inside.
pub fn resolve_binary(path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if p.extension().and_then(|e| e.to_str()) == Some("app") && p.is_dir() {
        let name = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let inner = p.join("Contents/MacOS").join(&name);
        if inner.exists() {
            return inner;
        }
    }
    p
}

/// The command-line flags for a proxy-wired pentest session. Pure, so it is
/// testable and easy to audit. `proxy` is like `http://127.0.0.1:8080`.
pub fn launch_args(proxy: &str, user_data_dir: &Path, extra_flags: &str, url: Option<&str>) -> Vec<String> {
    let mut args = vec![
        format!("--proxy-server={proxy}"),
        // Do not bypass loopback, so localhost apps are proxied and recorded too.
        "--proxy-bypass-list=<-loopback>".to_string(),
        // Accept the proxy's MITM certificates in this throwaway profile.
        "--ignore-certificate-errors".to_string(),
        "--allow-insecure-localhost".to_string(),
        // Suppress the yellow "unsupported command-line flag" infobar that Chrome
        // shows for --ignore-certificate-errors. This is the same flag automation
        // tools (ChromeDriver) use; it only hides the warning, the throwaway profile
        // is already isolated so nothing real is at risk.
        "--test-type".to_string(),
        // A dedicated, empty profile: no existing cookies/extensions, isolated.
        format!("--user-data-dir={}", user_data_dir.display()),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        "--new-window".to_string(),
    ];
    for f in extra_flags.split_whitespace() {
        args.push(f.to_string());
    }
    args.push(url.unwrap_or("about:blank").to_string());
    args
}

/// Launch the browser, wired to `proxy`. With `profile_dir` the browser uses that
/// PERSISTENT profile (cookies + history survive, e.g. a per-project profile next to
/// the `.ngintip`); without it, a fresh pid-scoped THROWAWAY profile is used (isolated,
/// swept on exit). Returns the spawned child so the app can close it when it quits.
pub fn launch(
    path: &str,
    proxy: &str,
    extra_flags: &str,
    url: Option<&str>,
    profile_dir: Option<&Path>,
) -> Result<std::process::Child, String> {
    if path.trim().is_empty() {
        return Err("no browser configured".into());
    }
    let bin = resolve_binary(path);
    if !bin.exists() {
        return Err(format!("browser not found at {}", bin.display()));
    }
    // Persistent per-project profile when given, else a per-instance throwaway: Chrome
    // locks a profile, so two Ngintip instances (or two windows of the same profile)
    // need separate dirs. The pid in the throwaway name also scopes orphan cleanup.
    let profile = match profile_dir {
        Some(p) => p.to_path_buf(),
        None => std::env::temp_dir().join(profile_marker()),
    };
    let _ = std::fs::create_dir_all(&profile);
    let args = launch_args(proxy, &profile, extra_flags, url);
    let mut cmd = std::process::Command::new(&bin);
    cmd.args(&args);
    // Put the browser in its own process group (it becomes the group leader), so the
    // app can bring down the browser AND its helper processes together on exit.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let child = cmd.spawn().map_err(|e| format!("failed to launch browser: {e}"))?;
    // Belt-and-suspenders: a detached watchdog kills the browser group the moment
    // THIS app process dies, covering exits where no in-app event fires (crash,
    // SIGKILL, `npm run tauri dev` restart). Window-close-while-app-stays (macOS) is
    // still handled by the app's window-close handler.
    #[cfg(unix)]
    spawn_death_watchdog(std::process::id(), child.id());
    Ok(child)
}

/// Spawn a tiny detached shell that waits until `app_pid` is gone, then kills the
/// browser's process group (`browser_pgid`, which equals the browser's own pid since
/// it was launched with process_group(0)).
#[cfg(unix)]
fn spawn_death_watchdog(app_pid: u32, browser_pgid: u32) {
    use std::os::unix::process::CommandExt;
    let script = format!(
        "while kill -0 {app} 2>/dev/null; do sleep 1; done; \
         kill -TERM -{pg} 2>/dev/null; sleep 1; kill -KILL -{pg} 2>/dev/null",
        app = app_pid,
        pg = browser_pgid,
    );
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c").arg(script);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Its own group, so terminating the browser group never takes the watchdog with it.
    cmd.process_group(0);
    let _ = cmd.spawn(); // fire and forget; it self-exits after killing the browser
}

/// The throwaway browser profile dir name for THIS app instance (pid-scoped, so
/// concurrent instances never share a profile or sweep each other's browsers).
fn profile_marker() -> String {
    format!("{}-browser-profile-{}", crate::brand::ID, std::process::id())
}

/// Kill every process (tracked or not) launched with THIS instance's throwaway
/// browser profile. Matches the unique `--user-data-dir` marker, so it works
/// regardless of PID tracking / process-group quirks, and never touches a browser
/// belonging to another running Ngintip instance.
pub fn kill_profile_browsers() {
    #[cfg(unix)]
    pkill_f(&profile_marker());
    // (Windows: relies on tracked-child termination; add taskkill here if needed.)
}

/// Startup cleanup: remove throwaway profiles left by PAST instances that are no
/// longer running (crash / SIGKILL that outran the watchdog), killing any browser
/// still using one. Profiles owned by a still-running instance are left alone, so
/// several Ngintip instances (and their browsers) coexist.
pub fn cleanup_orphan_browsers() {
    #[cfg(unix)]
    {
        let prefix = format!("{}-browser-profile-", crate::brand::ID);
        let me = std::process::id();
        let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else { return };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(pid_str) = name.strip_prefix(&prefix) else { continue };
            let Ok(pid) = pid_str.parse::<i32>() else { continue };
            // Keep our own, and any owned by another live instance (kill(pid,0) == 0).
            if pid as u32 == me || unsafe { libc::kill(pid, 0) } == 0 {
                continue;
            }
            pkill_f(name); // only matches that dead instance's browser
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// `pkill -f <pattern>`: SIGTERM everything whose full command line matches, then
/// SIGKILL any survivor. The pattern must be distinctive enough not to match this
/// app or its watchdog (the profile marker is).
#[cfg(unix)]
fn pkill_f(pattern: &str) {
    let run = |sig: &str| {
        let _ = std::process::Command::new("pkill").arg(sig).arg("-f").arg(pattern).status();
    };
    run("-TERM");
    std::thread::sleep(std::time::Duration::from_millis(200));
    run("-KILL");
}

/// Terminate a launched browser and its whole process group, so helper/renderer
/// processes go down with it. On Unix the browser leads its own group (see
/// `launch`), so signalling the group kills the tree; elsewhere the child is killed.
pub fn terminate(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        // child.id() == the browser's pgid (it was spawned with process_group(0)).
        let pid = child.id() as libc::pid_t;
        unsafe {
            libc::killpg(pid, libc::SIGTERM);
            libc::killpg(pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

// ---- download ---------------------------------------------------------------

/// GET a URL, following up to a few redirects, returning the final response.
async fn http_get(url: &str) -> Result<hyper::Response<hyper::body::Incoming>, String> {
    let client = crate::upstream::client(None, true).map_err(|e| e.to_string())?;
    let mut current = url.to_string();
    for _ in 0..5 {
        let uri: http::Uri = current.parse().map_err(|e| format!("bad url {current:?}: {e}"))?;
        let req = http::Request::builder()
            .uri(uri)
            .header(http::header::USER_AGENT, format!("{}/{}", crate::brand::NAME, env!("CARGO_PKG_VERSION")))
            .body(crate::body::empty())
            .map_err(|e| e.to_string())?;
        let res = client.request(req).await.map_err(|e| e.to_string())?;
        let status = res.status();
        if status.is_redirection() {
            if let Some(loc) = res.headers().get(http::header::LOCATION).and_then(|v| v.to_str().ok()) {
                current = loc.to_string();
                continue;
            }
        }
        if !status.is_success() {
            return Err(format!("download HTTP {status} for {current}"));
        }
        return Ok(res);
    }
    Err("too many redirects".into())
}

/// Download and extract the latest Chrome for Testing into `dir`, returning the
/// browser binary path. `progress(downloaded, total)` is called as bytes arrive.
pub async fn download(dir: &Path, progress: impl Fn(u64, Option<u64>)) -> Result<String, String> {
    use http_body_util::BodyExt;
    use std::io::Write;

    let platform = cft_platform().ok_or("auto-download is not supported on this OS/arch")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;

    // 1) Resolve the download URL from the CfT index.
    let index = http_get(CFT_JSON).await?;
    let bytes = index.into_body().collect().await.map_err(|e| e.to_string())?.to_bytes();
    let json: Value = serde_json::from_slice(&bytes).map_err(|e| format!("bad CfT index: {e}"))?;
    let url = pick_download_url(&json, platform).ok_or_else(|| format!("no Chrome for Testing build for {platform}"))?;

    // 2) Stream the zip to disk.
    let zip_path = dir.join("chrome-download.zip");
    let res = http_get(&url).await?;
    let total = res
        .headers()
        .get(http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let mut file = std::fs::File::create(&zip_path).map_err(|e| e.to_string())?;
    let mut body = res.into_body();
    let mut downloaded = 0u64;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|e| e.to_string())?;
        if let Some(data) = frame.data_ref() {
            file.write_all(data).map_err(|e| e.to_string())?;
            downloaded += data.len() as u64;
            progress(downloaded, total);
        }
    }
    file.flush().map_err(|e| e.to_string())?;
    drop(file);

    // 3) Extract with the OS tool, then locate the binary.
    extract_zip(&zip_path, dir)?;
    let _ = std::fs::remove_file(&zip_path);
    let bin = extracted_binary(dir, platform).ok_or("download extracted, but the browser binary was not found")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755));
    }
    Ok(bin.to_string_lossy().into_owned())
}

/// Extract `zip` into `dest` using the platform's own tool, which preserves app
/// bundles, symlinks and executable bits (the pure-Rust zip readers do not).
fn extract_zip(zip: &Path, dest: &Path) -> Result<(), String> {
    let status = if cfg!(target_os = "macos") {
        std::process::Command::new("ditto").arg("-x").arg("-k").arg(zip).arg(dest).status()
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command"])
            .arg(format!("Expand-Archive -Force -LiteralPath '{}' -DestinationPath '{}'", zip.display(), dest.display()))
            .status()
    } else {
        std::process::Command::new("unzip").arg("-o").arg(zip).arg("-d").arg(dest).status()
    };
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("extraction failed ({s})")),
        Err(e) => Err(format!("extraction tool not available: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cft_platform_is_known_here() {
        // The test host is one of the supported desktop platforms.
        let p = cft_platform();
        assert!(p.is_some(), "expected a CfT platform on this OS/arch");
        assert!(["mac-arm64", "mac-x64", "linux64", "win64"].contains(&p.unwrap()));
    }

    #[test]
    fn picks_the_download_url_for_a_platform() {
        let json = serde_json::json!({
            "channels": { "Stable": { "downloads": { "chrome": [
                { "platform": "linux64", "url": "https://example/chrome-linux64.zip" },
                { "platform": "mac-arm64", "url": "https://example/chrome-mac-arm64.zip" }
            ] } } }
        });
        assert_eq!(pick_download_url(&json, "mac-arm64").as_deref(), Some("https://example/chrome-mac-arm64.zip"));
        assert_eq!(pick_download_url(&json, "win64"), None);
        assert_eq!(pick_download_url(&serde_json::json!({}), "linux64"), None);
    }

    #[test]
    fn launch_args_wire_the_proxy_and_a_fresh_profile() {
        let dir = std::path::Path::new("/tmp/profile");
        let args = launch_args("http://127.0.0.1:8080", dir, "", Some("https://target.test"));
        assert!(args.contains(&"--proxy-server=http://127.0.0.1:8080".to_string()));
        assert!(args.contains(&"--ignore-certificate-errors".to_string()));
        // --test-type suppresses Chrome's "unsupported flag" infobar for the above.
        assert!(args.contains(&"--test-type".to_string()));
        assert!(args.iter().any(|a| a.starts_with("--user-data-dir=")));
        assert!(args.iter().any(|a| a.contains("<-loopback>")));
        assert_eq!(args.last().unwrap(), "https://target.test");
        // Extra flags are appended, no url defaults to about:blank.
        let args2 = launch_args("http://127.0.0.1:8080", dir, "--incognito --lang=en", None);
        assert!(args2.contains(&"--incognito".to_string()));
        assert!(args2.contains(&"--lang=en".to_string()));
        assert_eq!(args2.last().unwrap(), "about:blank");
    }

    #[test]
    fn candidates_are_absolute_paths() {
        for c in candidates() {
            assert!(c.is_absolute(), "candidate should be absolute: {}", c.display());
        }
    }

    #[test]
    fn launch_rejects_a_missing_binary() {
        assert!(launch("", "http://127.0.0.1:8080", "", None, None).is_err());
        assert!(launch("/no/such/browser/binary-xyz", "http://127.0.0.1:8080", "", None, None).is_err());
    }

    // Real test: a process launched with process_group(0) that forks a grandchild
    // (like a browser forks helpers) is fully taken down by terminate() — proving
    // the group-kill closes the whole browser tree, not just the launched PID.
    #[cfg(unix)]
    #[test]
    fn terminate_kills_the_whole_process_group() {
        use std::os::unix::process::CommandExt;
        let alive = |pid: i32| unsafe { libc::kill(pid, 0) } == 0;

        let mut cmd = std::process::Command::new("sh");
        // Fork a background grandchild, print its pid, then block. Both share the
        // group leader's pgid (this sh), because we make sh its own group leader.
        cmd.arg("-c").arg("sleep 30 & printf %s \"$!\" > \"$0\"; sleep 30")
            .arg(std::env::temp_dir().join(format!("ngintip-pgtest-{}.pid", std::process::id())));
        cmd.process_group(0);
        let mut child = cmd.spawn().expect("spawn sh");
        let leader = child.id() as i32;

        // Wait for the grandchild pid to be written.
        let pidfile = std::env::temp_dir().join(format!("ngintip-pgtest-{}.pid", std::process::id()));
        let mut grand = 0;
        for _ in 0..50 {
            if let Ok(s) = std::fs::read_to_string(&pidfile) {
                if let Ok(p) = s.trim().parse::<i32>() {
                    grand = p;
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(grand > 0, "grandchild pid was not captured");
        assert!(alive(leader), "leader should be running before terminate");
        assert!(alive(grand), "grandchild should be running before terminate");

        terminate(&mut child);

        // The grandchild (same process group) must be gone too.
        let mut gone = false;
        for _ in 0..50 {
            if !alive(grand) {
                gone = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(gone, "grandchild {grand} should be killed with the group");
        assert!(!alive(leader), "leader {leader} should be dead after terminate");
        let _ = std::fs::remove_file(&pidfile);
    }

    // Real test: pkill_f (the profile sweep) kills a process whose command line
    // contains the marker, even when it is not a tracked child (orphan cleanup).
    #[cfg(unix)]
    #[test]
    fn pkill_f_kills_untracked_process_by_pattern() {
        use std::time::Duration;
        // Unique marker so this only touches our test process, not real browsers.
        let marker = format!("ngintip-pkilltest-{}-marker", std::process::id());
        // Trailing builtin (`: # marker`) keeps sh from exec-replacing itself with
        // `sleep`, so the marker stays in the live process's command line (like a
        // browser keeps --user-data-dir=...marker... in its argv).
        let mut p = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("sleep 60; : # {marker}"))
            .spawn()
            .expect("spawn marked process");
        std::thread::sleep(Duration::from_millis(200));

        pkill_f(&marker);

        let mut dead = false;
        for _ in 0..80 {
            if let Ok(Some(_)) = p.try_wait() {
                dead = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = p.kill();
        let _ = p.wait();
        assert!(dead, "pkill_f should kill the untracked process matching the marker");
    }

    // Real test: the death-watchdog kills the browser's process group once the app
    // process it was told to watch has exited (covers crash / SIGKILL / dev restart).
    #[cfg(unix)]
    #[test]
    fn watchdog_kills_browser_group_when_app_dies() {
        use std::os::unix::process::CommandExt;
        use std::time::Duration;
        let alive = |pid: i32| unsafe { libc::kill(pid, 0) } == 0;

        // A stand-in "app" process the watchdog will watch.
        let mut app = std::process::Command::new("sh").arg("-c").arg("sleep 60").spawn().expect("spawn app");
        let app_pid = app.id();

        // A stand-in "browser": its own group leader that forks a grandchild.
        let pidfile = std::env::temp_dir().join(format!("ngintip-wdtest-{}.pid", std::process::id()));
        let mut b = std::process::Command::new("sh");
        b.arg("-c").arg("sleep 60 & printf %s \"$!\" > \"$0\"; sleep 60")
            .arg(&pidfile)
            .process_group(0);
        let mut browser = b.spawn().expect("spawn browser");
        let browser_pgid = browser.id();

        let mut grand = 0;
        for _ in 0..50 {
            if let Ok(s) = std::fs::read_to_string(&pidfile) {
                if let Ok(p) = s.trim().parse::<i32>() {
                    grand = p;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(grand > 0, "grandchild pid was not captured");

        spawn_death_watchdog(app_pid, browser_pgid);

        // Kill the watched app; the watchdog must then take the browser group down.
        let _ = app.kill();
        let _ = app.wait();

        let (mut leader_dead, mut grand_dead) = (false, false);
        for _ in 0..120 {
            if let Ok(Some(_)) = browser.try_wait() {
                leader_dead = true;
            }
            if !alive(grand) {
                grand_dead = true;
            }
            if leader_dead && grand_dead {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        // Best-effort cleanup before asserting.
        let _ = browser.kill();
        let _ = browser.wait();
        let _ = std::fs::remove_file(&pidfile);
        assert!(leader_dead, "watchdog should kill the browser leader after the app dies");
        assert!(grand_dead, "watchdog should kill the browser grandchild after the app dies");
    }
}
