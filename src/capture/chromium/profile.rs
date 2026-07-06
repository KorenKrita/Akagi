//! Profile directory resolution and singleton-lock reclamation.
//!
//! Chromium refuses to start a second instance against a `--user-data-dir`
//! whose `SingletonLock` (Unix symlink) / `SingletonLock` (Windows file)
//! points at a live PID. Instead of launching, the second process hands its
//! start URL to the running browser (a duplicate tab) and exits — leaving our
//! capture with no DevTools endpoint. So before we spawn, we must guarantee
//! the profile is free.
//!
//! `reclaim_singleton` reads the lock and identifies the prior PID:
//! - dead PID (a previous run was SIGKILLed / OOMed / lost power) → remove the
//!   stale lock files and launch fresh.
//! - live PID (the user closed Akagi but left the controlled browser open) →
//!   terminate that browser (staged SIGTERM → SIGKILL), wait for it to exit,
//!   then remove the lock. We own this profile, so reclaiming it is safe; the
//!   relaunch reuses the same profile dir, so login/cookies are preserved and
//!   Mahjong Soul reconnects to an in-progress match on reload.
//!
//! Running two Akagi instances against the same profile is unsupported — they
//! would fight over the lock.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
#[cfg(windows)]
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
#[cfg(windows)]
use crate::util::NoConsoleWindow;
use tracing::{debug, info, warn};

/// How long to wait for a polite SIGTERM/`taskkill` to take effect before
/// escalating to SIGKILL/`taskkill /F`.
const TERM_GRACE: Duration = Duration::from_secs(5);
/// How long to wait for the forced kill to take effect.
const KILL_GRACE: Duration = Duration::from_secs(2);
/// Poll cadence while waiting for the owner process to disappear.
const KILL_POLL: Duration = Duration::from_millis(100);

/// Resolve the user-data-dir path for the controlled Chromium instance.
/// `configured` empty → exe-adjacent `chrome-profile/` via
/// [`crate::util::resolve_dir`], so a portable zip keeps everything
/// (config, logs, profile) in one folder. Otherwise `configured` is
/// treated as an absolute path (relative paths are not supported here
/// — there's no meaningful root for them).
pub fn resolve_profile_dir(configured: &str) -> Result<PathBuf> {
    if !configured.is_empty() {
        let p = PathBuf::from(configured);
        if !p.is_absolute() {
            return Err(anyhow!(
                "capture.chromium.user_data_dir must be absolute (got {configured:?})"
            ));
        }
        return Ok(p);
    }
    Ok(crate::util::resolve_dir(Path::new("./chrome-profile")))
}

/// Make the profile dir launchable by clearing any `SingletonLock` /
/// `SingletonSocket` / `SingletonCookie` — terminating the owning browser
/// first if it is still alive (see module docs). Returns `Err` only when a
/// live owner could not be terminated; the caller surfaces that to the user.
///
/// Blocking (it may sleep while waiting for the owner to exit); call it off
/// the async runtime via `spawn_blocking`.
pub fn reclaim_singleton(profile: &Path) -> Result<()> {
    // On Windows, Chrome writes no `SingletonLock` file (it uses a named mutex +
    // hidden message window), so the lock-file path below can't see a surviving
    // controlled browser — a second `--user-data-dir` launch would just open a
    // duplicate tab in it, leaving our relaunch with no DevTools endpoint. So on
    // Windows we locate that browser by its command-line `--user-data-dir` and
    // terminate it first. On Unix, Chrome *does* write SingletonLock, so
    // `reclaim_singleton_inner` already handles the live owner — that proven
    // path is left untouched (this block is compiled out on non-Windows).
    #[cfg(windows)]
    {
        if let Some(pid) = find_controlled_browser_pid(profile) {
            info!(
                "terminating existing controlled chromium pid {pid} to reclaim profile {}",
                profile.display()
            );
            if !terminate_pid_windows(pid) {
                return Err(anyhow!(
                    "couldn't terminate the browser already using profile {} (pid {pid}) — \
                     close it manually and click Restart",
                    profile.display()
                ));
            }
        }
    }
    if !profile.exists() {
        return Ok(()); // fresh dir, nothing else to clean
    }
    reclaim_singleton_inner(profile)
}

/// True when a process with this `name` and command-line `cmd` is the **browser
/// process** of the controlled Chromium for `profile`: it carries our
/// `--user-data-dir=<profile>` and is not a `--type=…` renderer/GPU child
/// (killing the browser process takes its children down with it). Pure so the
/// matching is unit-testable without a live browser.
///
/// Windows-only: this is the reclaim mechanism there because Chrome writes no
/// SingletonLock file. Unix uses the lock symlink instead (see module docs).
#[cfg(windows)]
fn is_controlled_browser(name: &str, cmd: &[String], profile: &Path) -> bool {
    let name = name.to_ascii_lowercase();
    if !name.contains("chrome") && !name.contains("chromium") {
        return false;
    }
    let needle = format!("--user-data-dir={}", profile.display());
    let has_profile = cmd.iter().any(|a| a.contains(needle.as_str()));
    let is_child = cmd.iter().any(|a| a.starts_with("--type="));
    has_profile && !is_child
}

/// Find the PID of the controlled Chromium **browser** process for `profile`,
/// if one is still running, by matching the process command line via `sysinfo`.
/// Windows-only (Unix reclaims via the SingletonLock symlink instead).
#[cfg(windows)]
fn find_controlled_browser_pid(profile: &Path) -> Option<u32> {
    let mut sys = System::new();
    // The default refresh does NOT fetch the command line; ask for it
    // explicitly (that's what we match `--user-data-dir` against).
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
    );
    for proc in sys.processes().values() {
        let name = proc.name().to_string_lossy();
        let cmd: Vec<String> = proc
            .cmd()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        if is_controlled_browser(name.as_ref(), &cmd, profile) {
            return Some(proc.pid().as_u32());
        }
    }
    None
}

/// Best-effort removal of the three singleton marker files. A clean browser
/// shutdown removes its own lock, so a `NotFound` here is expected and benign.
fn remove_singleton_files(profile: &Path) {
    for name in ["SingletonLock", "SingletonSocket", "SingletonCookie"] {
        let p = profile.join(name);
        match std::fs::remove_file(&p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => warn!("failed to remove {}: {e}", p.display()),
        }
    }
}

#[cfg(unix)]
fn reclaim_singleton_inner(profile: &Path) -> Result<()> {
    let lock = profile.join("SingletonLock");
    let metadata = match std::fs::symlink_metadata(&lock) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(anyhow!(
                "failed to stat singleton lock {}: {e}",
                lock.display()
            ));
        }
    };
    if !metadata.file_type().is_symlink() {
        // Unexpected — not a singleton lock we recognise. Leave alone.
        debug!(
            "singleton path {} is not a symlink; leaving untouched",
            lock.display()
        );
        return Ok(());
    }
    let target = std::fs::read_link(&lock)
        .with_context(|| format!("reading singleton symlink {}", lock.display()))?;
    let target_str = target.to_string_lossy();
    // target format is "<hostname>-<pid>"
    let pid = target_str
        .rsplit_once('-')
        .and_then(|(_, n)| n.parse::<i32>().ok());
    let Some(pid) = pid else {
        warn!(
            "singleton lock target {} doesn't match expected <host>-<pid>; leaving alone",
            target_str
        );
        return Ok(());
    };
    if process_alive_unix(pid) {
        // A browser we previously launched is still running with our profile
        // (the user closed Akagi but left Chrome open). Terminate it so we can
        // relaunch a single fresh instance — a second `--user-data-dir` launch
        // would only hand its start URL to the live browser (a duplicate tab)
        // and then exit, leaving capture with no DevTools endpoint.
        info!("terminating existing chromium pid {pid} to relaunch (target={target_str})");
        if !terminate_pid_unix(pid) {
            return Err(anyhow!(
                "couldn't terminate the browser already using profile {} (pid {pid}) — \
                 close it manually and click Restart",
                profile.display()
            ));
        }
    } else {
        info!(
            "removing stale chromium singleton lock {} → {} (pid {pid} gone)",
            lock.display(),
            target_str
        );
    }
    remove_singleton_files(profile);
    Ok(())
}

#[cfg(unix)]
fn process_alive_unix(pid: i32) -> bool {
    // Shell out to `kill -0 <pid>` — POSIX signal-0 probe. Exit 0 means
    // alive (or alive-but-unsignalable, which we treat the same: don't
    // touch the lock). Avoids pulling libc in directly. Absolute path
    // is consistent across Linux and macOS — no PATH ambiguity.
    let status = std::process::Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .status();
    match status {
        Ok(s) => s.success(),
        Err(_) => false,
    }
}

/// Staged terminate of an arbitrary PID: polite SIGTERM (so Chrome can clean
/// up its own lock files), wait, then SIGKILL. Returns `true` once the process
/// is confirmed gone. Mirrors [`super::launch::terminate`] but for a raw PID,
/// since the owner is not a `Child` of this process.
#[cfg(unix)]
fn terminate_pid_unix(pid: i32) -> bool {
    let _ = std::process::Command::new("/bin/kill")
        .args(["-TERM", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .status();
    if wait_until_dead_unix(pid, TERM_GRACE) {
        return true;
    }
    warn!("chromium pid {pid} did not exit after SIGTERM, sending SIGKILL");
    let _ = std::process::Command::new("/bin/kill")
        .args(["-KILL", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .status();
    wait_until_dead_unix(pid, KILL_GRACE)
}

#[cfg(unix)]
fn wait_until_dead_unix(pid: i32, grace: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < grace {
        if !process_alive_unix(pid) {
            return true;
        }
        std::thread::sleep(KILL_POLL);
    }
    !process_alive_unix(pid)
}

#[cfg(windows)]
fn reclaim_singleton_inner(profile: &Path) -> Result<()> {
    let lock = profile.join("SingletonLock");
    let pid = match std::fs::read_to_string(&lock) {
        Ok(s) => s.trim().parse::<u32>().ok(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(anyhow!(
                "failed to read singleton lock {}: {e}",
                lock.display()
            ));
        }
    };
    if let Some(pid) = pid {
        if process_alive_windows(pid) {
            info!("terminating existing chromium pid {pid} to relaunch");
            if !terminate_pid_windows(pid) {
                return Err(anyhow!(
                    "couldn't terminate the browser already using profile {} (pid {pid}) — \
                     close it manually and click Restart",
                    profile.display()
                ));
            }
        } else {
            info!(
                "removing stale chromium singleton lock {} (pid {pid} gone)",
                lock.display()
            );
        }
    }
    remove_singleton_files(profile);
    Ok(())
}

#[cfg(windows)]
fn process_alive_windows(pid: u32) -> bool {
    // Use `tasklist /FI "PID eq <pid>"` — no extra crate, returns "INFO: No tasks…"
    // line if the PID is gone. Avoids pulling in `windows-sys` for one syscall.
    let out = match std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .no_console_window()
        .output()
    {
        Ok(o) => o,
        Err(_) => return false,
    };
    let s = String::from_utf8_lossy(&out.stdout);
    s.trim().lines().any(|line| line.contains(&pid.to_string()))
}

/// Staged terminate of an arbitrary PID on Windows: polite `taskkill`, wait,
/// then `taskkill /F`. Returns `true` once the process is confirmed gone.
#[cfg(windows)]
fn terminate_pid_windows(pid: u32) -> bool {
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .no_console_window()
        .status();
    if wait_until_dead_windows(pid, TERM_GRACE) {
        return true;
    }
    warn!("chromium pid {pid} did not exit after taskkill, forcing /F");
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .no_console_window()
        .status();
    wait_until_dead_windows(pid, KILL_GRACE)
}

#[cfg(windows)]
fn wait_until_dead_windows(pid: u32, grace: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < grace {
        if !process_alive_windows(pid) {
            return true;
        }
        std::thread::sleep(KILL_POLL);
    }
    !process_alive_windows(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_default_lands_at_chrome_profile() {
        let p = resolve_profile_dir("").unwrap();
        // ends with chrome-profile regardless of which arm of resolve_dir
        // fired (exe-adjacent on portable, user_root on AppImage).
        assert!(
            p.ends_with("chrome-profile"),
            "expected ending in chrome-profile: {}",
            p.display()
        );
    }

    #[test]
    fn resolve_explicit_absolute() {
        let abs = if cfg!(windows) {
            r"C:\tmp\custom"
        } else {
            "/tmp/custom"
        };
        let p = resolve_profile_dir(abs).unwrap();
        assert_eq!(p, PathBuf::from(abs));
    }

    #[test]
    fn resolve_relative_rejected() {
        let r = resolve_profile_dir("./relative");
        assert!(r.is_err());
    }

    #[test]
    fn reclaim_no_lock_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        reclaim_singleton(dir.path()).unwrap();
    }

    /// Regression for the Windows duplicate-tab / DevToolsActivePort-timeout
    /// bug: the surviving controlled browser must be identified by its
    /// command-line `--user-data-dir` (Chrome writes no SingletonLock on
    /// Windows), matching the browser process but not its renderer children or
    /// an unrelated Chrome on a different profile.
    #[cfg(windows)]
    #[test]
    fn is_controlled_browser_matches_browser_not_children() {
        let profile = Path::new(if cfg!(windows) {
            r"C:\p\chrome-profile"
        } else {
            "/p/chrome-profile"
        });
        let udir = format!("--user-data-dir={}", profile.display());

        // Browser process: our --user-data-dir, no --type.
        assert!(is_controlled_browser(
            "chrome.exe",
            &[udir.clone(), "--remote-debugging-port=1234".into()],
            profile
        ));
        assert!(is_controlled_browser("chromium", &[udir.clone()], profile));

        // Renderer/GPU child: has --type → not the process we target directly.
        assert!(!is_controlled_browser(
            "chrome.exe",
            &[udir.clone(), "--type=renderer".into()],
            profile
        ));

        // A chrome using a *different* profile is not ours.
        let other = format!(
            "--user-data-dir={}",
            Path::new(if cfg!(windows) { r"C:\other" } else { "/other" }).display()
        );
        assert!(!is_controlled_browser("chrome.exe", &[other], profile));

        // A non-chrome process is never matched, even with the arg.
        assert!(!is_controlled_browser("firefox", &[udir], profile));
    }

    /// Real end-to-end check (run with `--ignored` and `AKAGI_TEST_CHROME` set
    /// to a chrome/chromium binary): launch an actual headless browser with a
    /// temp profile, confirm `find_controlled_browser_pid` locates it via the
    /// live process command line (exercising the sysinfo `with_cmd` refresh),
    /// and that `reclaim_singleton` terminates it. Skipped when the env var is
    /// unset so CI without a browser stays green.
    #[cfg(windows)]
    #[test]
    #[ignore = "needs a real browser binary via AKAGI_TEST_CHROME"]
    fn find_and_reclaim_real_browser() {
        let Ok(chrome) = std::env::var("AKAGI_TEST_CHROME") else {
            eprintln!("AKAGI_TEST_CHROME not set; skipping");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path();
        let mut child = std::process::Command::new(&chrome)
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg("--remote-debugging-port=0")
            .arg("--headless=new")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .spawn()
            .expect("spawn browser");
        std::thread::sleep(Duration::from_secs(3));

        let found = find_controlled_browser_pid(profile);
        assert!(
            found.is_some(),
            "find_controlled_browser_pid should locate the browser by its command line"
        );

        reclaim_singleton(profile).expect("reclaim should terminate the browser");
        std::thread::sleep(Duration::from_millis(500));
        assert!(
            find_controlled_browser_pid(profile).is_none(),
            "browser should be gone after reclaim"
        );

        let _ = child.kill();
        let _ = child.wait();
    }

    #[cfg(unix)]
    #[test]
    fn reclaim_unlinks_dead_pid() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("SingletonLock");
        // i32::MAX — a valid (parseable) PID far past any realistic
        // pid_max, so it is certainly dead.
        symlink("akagi-test-host-2147483647", &lock).unwrap();
        reclaim_singleton(dir.path()).unwrap();
        assert!(
            std::fs::symlink_metadata(&lock).is_err(),
            "stale lock should have been unlinked"
        );
    }

    /// Regression for the duplicate-tab / DevToolsActivePort-timeout bug:
    /// reopening Akagi while a controlled browser is still running must
    /// terminate that browser and clear the lock so a fresh instance can
    /// launch (rather than handing a duplicate tab to the live browser).
    #[cfg(unix)]
    #[test]
    fn reclaim_kills_live_owner_and_clears_lock() {
        use std::io::Read;
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("SingletonLock");

        // Spawn `sleep` detached so it is reparented to init and reaped
        // there when killed — mirrors a real browser that outlived the Akagi
        // process that launched it (avoids leaving a zombie we'd own, which
        // `kill -0` would still report as alive). Redirect the child's
        // stdout/stderr to /dev/null so it doesn't inherit (and hold open)
        // the pipe we read `$!` from — otherwise `read_to_string` blocks
        // until `sleep` itself exits.
        let mut launcher = std::process::Command::new("sh")
            .args(["-c", "sleep 60 >/dev/null 2>&1 & echo $!"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn launcher");
        let mut out = String::new();
        launcher
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut out)
            .unwrap();
        let _ = launcher.wait(); // reap the short-lived shell
        let pid: i32 = out.trim().parse().expect("background sleep pid");

        assert!(process_alive_unix(pid), "detached sleep should be alive");
        symlink(format!("akagi-test-host-{pid}"), &lock).unwrap();

        reclaim_singleton(dir.path()).unwrap();

        assert!(
            !process_alive_unix(pid),
            "live owner process should have been terminated"
        );
        assert!(
            std::fs::symlink_metadata(&lock).is_err(),
            "singleton lock should be removed after reclaim"
        );
    }
}
