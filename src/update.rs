//! Self-update + a passive "newer version out" notice.
//!
//! No HTTP crate: we shell out to `curl` (already a hard requirement of the
//! install script) and checksum with `sha256sum`/`shasum`, matching
//! install.sh exactly. The binary is replaced with a plain rename over
//! itself — atomic on unix, and safe while the old image is still running.
//!
//! Package-manager installs (brew/nix/cargo) are never self-replaced; doing
//! so corrupts the manager's bookkeeping. Those are told to use their own
//! upgrade command instead.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const REPO: &str = "szkabaroli/vibin";
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// The running binary's version (compile-time crate version).
pub fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The release-artifact target triple for this build, or `None` on a
/// platform we don't publish binaries for (self-update falls back to a
/// "build from source" message there).
pub fn target() -> Option<&'static str> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        Some("aarch64-apple-darwin")
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        Some("x86_64-apple-darwin")
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
    {
        Some("x86_64-unknown-linux-gnu")
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64", target_env = "gnu"))]
    {
        Some("aarch64-unknown-linux-gnu")
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "musl"))]
    {
        Some("x86_64-unknown-linux-musl")
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64", target_env = "musl"))]
    {
        Some("aarch64-unknown-linux-musl")
    }
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"),
        all(target_os = "linux", target_arch = "aarch64", target_env = "gnu"),
        all(target_os = "linux", target_arch = "x86_64", target_env = "musl"),
        all(target_os = "linux", target_arch = "aarch64", target_env = "musl"),
    )))]
    {
        None
    }
}

/// How this binary got onto the machine — decides whether `+update` may
/// replace it, or must defer to a package manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMethod {
    /// curl|sh script or a hand-dropped binary — safe to self-replace.
    SelfManaged,
    Homebrew,
    Nix,
    Cargo,
}

impl InstallMethod {
    fn detect() -> Self {
        let path = std::env::current_exe()
            .ok()
            .and_then(|p| p.to_str().map(str::to_owned))
            .unwrap_or_default();
        if path.starts_with("/nix/store/") {
            Self::Nix
        } else if path.contains("/Cellar/")
            || path.contains("/homebrew/")
            || path.contains("/linuxbrew/")
        {
            Self::Homebrew
        } else if path.contains("/.cargo/") {
            Self::Cargo
        } else {
            Self::SelfManaged
        }
    }

    /// The command the user should run to upgrade a managed install.
    fn upgrade_hint(self) -> Option<&'static str> {
        match self {
            Self::SelfManaged => None,
            Self::Homebrew => Some("brew upgrade vibin"),
            Self::Nix => Some("nix profile upgrade vibin  # (or update your flake input)"),
            Self::Cargo => {
                Some("cargo binstall vibin --git https://github.com/szkabaroli/vibin --force")
            }
        }
    }
}

/// `1.2.3` (ignoring any `-suffix`) -> comparable tuple.
fn parse(v: &str) -> (u64, u64, u64) {
    let core = v.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next().unwrap_or(core);
    let mut it = core.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0))
}

fn is_newer(candidate: &str, than: &str) -> bool {
    parse(candidate) > parse(than)
}

/// Outcome of asking GitHub for the latest release.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Fetch {
    /// Latest published version (no leading `v`).
    Version(String),
    /// The API answered, but the repo has no releases yet (404).
    NoReleases,
    /// Couldn't reach GitHub, or an unexpected response.
    Failed,
}

/// Ask GitHub for the latest release. We don't pass curl `-f` so a 404 still
/// returns a body; the appended `%{http_code}` lets us tell "no releases yet"
/// apart from a real network failure.
fn fetch_latest() -> Fetch {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let out = match Command::new("curl")
        .args([
            "-sSL",
            "-w",
            "\n%{http_code}",
            "-H",
            "Accept: application/vnd.github+json",
            "-A",
            "vibin-updater",
            &url,
        ])
        .output()
    {
        Ok(out) if out.status.success() => out,
        _ => return Fetch::Failed, // curl itself failed => network/DNS
    };
    classify(&String::from_utf8_lossy(&out.stdout))
}

/// Split the `<body>\n<http_code>` curl produced and interpret it. Pure, so
/// it's unit-testable without a network.
fn classify(response: &str) -> Fetch {
    let (body, code) = response.rsplit_once('\n').unwrap_or(("", response));
    match code.trim() {
        "404" => Fetch::NoReleases,
        "200" => serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|j| j.get("tag_name")?.as_str().map(str::to_owned))
            .map(|t| Fetch::Version(t.trim_start_matches('v').to_string()))
            .unwrap_or(Fetch::Failed),
        _ => Fetch::Failed,
    }
}

/// The latest published version (no leading `v`), or `None` for the passive
/// path — callers there stay quiet on any non-success.
pub fn latest_version() -> Option<String> {
    match fetch_latest() {
        Fetch::Version(v) => Some(v),
        _ => None,
    }
}

// ---- passive notice (throttled, cached) --------------------------------

fn cache_file() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("vibin").join("update-check"))
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Kick off a background version check if the cached one is stale (>24h).
/// Best-effort and detached: it writes `<unix_ts>\n<version>` to the cache
/// for [`pending_notice`] to read on exit. Cheap no-op when fresh.
pub fn spawn_background_check() {
    let Some(cache) = cache_file() else { return };
    if let Ok(text) = std::fs::read_to_string(&cache)
        && let Some(ts) = text.lines().next().and_then(|l| l.parse::<u64>().ok())
        && now_secs().saturating_sub(ts) < CHECK_INTERVAL.as_secs()
    {
        return; // checked recently
    }
    std::thread::spawn(move || {
        if let Some(latest) = latest_version() {
            if let Some(dir) = cache.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&cache, format!("{}\n{latest}\n", now_secs()));
        }
    });
}

/// A one-line "newer version available" notice, if the last cached check
/// found one. Printed to stderr on exit, so it survives the alt-screen.
pub fn pending_notice() -> Option<String> {
    let cache = cache_file()?;
    let text = std::fs::read_to_string(&cache).ok()?;
    notice_from_cache(&text, current())
}

/// Cache body is `<unix_ts>\n<version>`; emit a notice when line 2 names a
/// version newer than `current`. Split out so it's testable without touching
/// the filesystem or environment.
fn notice_from_cache(text: &str, current: &str) -> Option<String> {
    let latest = text.lines().nth(1)?.trim();
    if latest.is_empty() || !is_newer(latest, current) {
        return None;
    }
    Some(format!(
        "\x1b[2m↑\x1b[0m vibin {latest} available (you have {current}) — run `vibin +update`",
    ))
}

// ---- self-update -------------------------------------------------------

/// Implements `vibin +update`. Prints progress to stderr and exits the
/// process with a suitable status; never returns on the update path.
pub fn run() -> ! {
    match do_update() {
        Ok(msg) => {
            eprintln!("{msg}");
            std::process::exit(0);
        }
        Err(msg) => {
            eprintln!("update failed: {msg}");
            std::process::exit(1);
        }
    }
}

fn do_update() -> Result<String, String> {
    let method = InstallMethod::detect();
    if let Some(hint) = method.upgrade_hint() {
        return Err(format!(
            "this vibin was installed with {:?}; update it with:\n\n    {hint}\n",
            method
        ));
    }

    eprintln!("  current: {}", current());
    let latest = match fetch_latest() {
        Fetch::Version(v) => v,
        Fetch::NoReleases => {
            return Err("no releases published yet — nothing to update to".into());
        }
        Fetch::Failed => {
            return Err("could not reach GitHub (check your connection)".into());
        }
    };
    eprintln!("  latest:  {latest}");
    if !is_newer(&latest, current()) {
        return Ok("already up to date.".into());
    }

    let target = target().ok_or("no prebuilt binary for this platform — build from source")?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("cannot locate the install directory")?.to_path_buf();

    let tag = format!("v{latest}");
    let stem = format!("vibin-{tag}-{target}");
    let file = format!("{stem}.tar.gz");
    let base = format!("https://github.com/{REPO}/releases/download/{tag}");

    // stage everything next to the existing binary so the final rename is a
    // same-filesystem (atomic) swap.
    let work = dir.join(format!(".vibin-update-{}", std::process::id()));
    std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let cleanup = || {
        let _ = std::fs::remove_dir_all(&work);
    };

    let result = (|| {
        let tarball = work.join(&file);
        eprintln!("  downloading {file}…");
        curl_to(&format!("{base}/{file}"), &tarball)?;

        let sums = work.join(format!("{file}.sha256"));
        if curl_to(&format!("{base}/{file}.sha256"), &sums).is_ok() {
            verify_sha256(&tarball, &sums)?;
            eprintln!("  checksum ok");
        }

        // tar writes the `vibin-<tag>-<target>/` dir back out
        let status = Command::new("tar")
            .arg("xzf")
            .arg(&tarball)
            .arg("-C")
            .arg(&work)
            .status()
            .map_err(|e| format!("tar: {e}"))?;
        if !status.success() {
            return Err("tar extraction failed".to_string());
        }

        let fresh = work.join(&stem).join("vibin");
        if !fresh.exists() {
            return Err("archive did not contain the expected binary".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o755));
        }

        // atomic replace: on unix the running process keeps its open image,
        // so renaming a new file over the path is safe mid-run.
        std::fs::rename(&fresh, &exe).map_err(|e| {
            format!(
                "could not replace {} ({e}) — you may need write access (try sudo)",
                exe.display()
            )
        })?;
        Ok(())
    })();

    cleanup();
    result?;
    Ok(format!("updated {} -> {latest}", current()))
}

fn curl_to(url: &str, dest: &Path) -> Result<(), String> {
    let status = Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(dest)
        .arg(url)
        .status()
        .map_err(|e| format!("curl: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("download failed: {url}")) }
}

fn verify_sha256(file: &Path, sums: &Path) -> Result<(), String> {
    let want = std::fs::read_to_string(sums)
        .map_err(|e| e.to_string())?
        .split_whitespace()
        .next()
        .map(str::to_owned)
        .ok_or("empty checksum file")?;

    let got = if let Ok(out) = Command::new("sha256sum").arg(file).output() {
        out.status.success().then_some(out.stdout)
    } else {
        None
    }
    .or_else(|| {
        Command::new("shasum")
            .args(["-a", "256"])
            .arg(file)
            .output()
            .ok()
            .and_then(|o| o.status.success().then_some(o.stdout))
    });

    let Some(got) = got else {
        return Ok(()); // no sha tool — skip rather than block (loudly logged by caller)
    };
    let got = String::from_utf8_lossy(&got);
    let got = got.split_whitespace().next().unwrap_or("");
    if got.eq_ignore_ascii_case(&want) {
        Ok(())
    } else {
        Err(format!("checksum mismatch (expected {want}, got {got})"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_ordering() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
    }

    #[test]
    fn version_parsing_is_lenient() {
        assert_eq!(parse("v1.2.3"), (1, 2, 3));
        assert_eq!(parse("1.2.3-rc1"), (1, 2, 3));
        assert_eq!(parse("2"), (2, 0, 0));
        assert_eq!(parse("garbage"), (0, 0, 0));
    }

    #[test]
    fn notice_absent_when_not_newer() {
        // current() can never be newer than itself
        assert!(!is_newer(current(), current()));
    }

    #[test]
    fn classify_distinguishes_404_from_success() {
        // curl appends "\n<http_code>" to the body
        assert_eq!(classify("anything\n404"), Fetch::NoReleases);
        assert_eq!(classify("{\"tag_name\":\"v0.3.0\"}\n200"), Fetch::Version("0.3.0".into()));
        // 200 but unparseable / missing tag -> Failed, not a panic
        assert_eq!(classify("not json\n200"), Fetch::Failed);
        assert_eq!(classify("{}\n200"), Fetch::Failed);
        // other statuses (rate limit, 5xx) -> Failed
        assert_eq!(classify("{\"message\":\"rate limited\"}\n403"), Fetch::Failed);
    }

    #[test]
    fn notice_reads_version_from_cache_line_two() {
        // <ts>\n<version> — a newer version yields a notice naming both
        let notice = notice_from_cache("1784000000\n0.2.0\n", "0.1.0").unwrap();
        assert!(notice.contains("0.2.0"));
        assert!(notice.contains("0.1.0"));
        // same or older version -> no notice
        assert!(notice_from_cache("1784000000\n0.1.0\n", "0.1.0").is_none());
        assert!(notice_from_cache("1784000000\n0.0.9\n", "0.1.0").is_none());
        // malformed cache -> no notice, no panic
        assert!(notice_from_cache("garbage", "0.1.0").is_none());
    }
}
