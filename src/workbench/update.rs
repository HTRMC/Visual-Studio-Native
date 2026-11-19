//! Updates from the GitHub releases, after VS Code's: a check soon after
//! start and every few hours while `update.mode` is "default" (Help > Check
//! for Updates... any time). An installed vsn downloads the new User Setup in
//! the background, checks it against the release's SHA256SUMS.txt, and asks
//! to restart; the setup runs silently once vsn has quit and starts it again.
//! A portable vsn (no uninstaller beside it) is pointed at the release page.

use std::{
    fs,
    io::{Read, Write},
    path::PathBuf,
    rc::Rc,
    sync::Mutex,
    time::Duration,
};

use gpui_kit::*;
use sha2::{Digest, Sha256};

use super::{NoteAction, Quit, Severity, Workbench, terminal};

const LATEST: &str = "https://api.github.com/repos/HTRMC/Visual-Studio-Native/releases/latest";
/// Not right at start, so the first check doesn't compete with opening the folder.
const FIRST_CHECK: Duration = Duration::from_secs(30);
const EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// The downloaded setup to run once vsn quits.
static PENDING: Mutex<Option<PathBuf>> = Mutex::new(None);

#[derive(Default)]
pub(super) struct Update {
    timer: Option<Task<()>>,
    checking: bool,
    /// The version this session already offered, so the next check doesn't
    /// ask again.
    offered: Option<String>,
}

/// The newest release: its version, page, and setup's name and URL.
struct Release {
    version: String,
    page: String,
    setup: Option<(String, String)>,
    sums: Option<String>,
}

/// "v0.1.2-1.95.0" or "0.1.2" as (0, 1, 2); the part after `-` is the VS
/// Code API version vsn was built against, not vsn's.
fn version(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.trim_start_matches('v').split('-').next()?;
    let mut parts = core.split('.').map(|p| p.parse().ok());
    Some((
        parts.next()??,
        parts.next()??,
        parts.next().flatten().unwrap_or(0),
    ))
}

fn newer(release: &str, running: &str) -> bool {
    matches!((version(release), version(running)), (Some(r), Some(v)) if r > v)
}

fn mode() -> String {
    (terminal::settings()["update.mode"].as_str())
        .unwrap_or("default")
        .to_string()
}

/// Installed by the setup, which leaves its uninstaller beside vsn.exe.
fn installed() -> bool {
    cfg!(windows)
        && std::env::current_exe()
            .ok()
            .and_then(|e| Some(e.parent()?.join("unins000.exe").exists()))
            .unwrap_or(false)
}

fn get(url: &str) -> Result<String, String> {
    ureq::get(url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "vsn-update")
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())
}

fn latest() -> Result<Release, String> {
    let json: serde_json::Value = serde_json::from_str(&get(LATEST)?).map_err(|e| e.to_string())?;
    let tag = json["tag_name"].as_str().ok_or("The release has no tag")?;
    let assets = json["assets"].as_array().cloned().unwrap_or_default();
    let asset = |pick: &dyn Fn(&str) -> bool| {
        assets.iter().find_map(|a| {
            let name = a["name"].as_str().filter(|n| pick(n))?;
            Some((name.to_string(), a["browser_download_url"].as_str()?.to_string()))
        })
    };
    Ok(Release {
        version: tag.trim_start_matches('v').split('-').next().unwrap_or(tag).to_string(),
        page: json["html_url"].as_str().unwrap_or(LATEST).to_string(),
        setup: asset(&|n| n.starts_with("VSNUserSetup-x64-") && n.ends_with(".exe"))
            .filter(|_| cfg!(target_arch = "x86_64")),
        sums: asset(&|n| n == "SHA256SUMS.txt").map(|(_, url)| url),
    })
}

/// The release's setup in the temp folder, checked against its SHA-256 in
/// SHA256SUMS.txt; one already downloaded and matching is kept.
fn download(release: &Release) -> Result<PathBuf, String> {
    let (name, url) = release.setup.as_ref().ok_or("The release has no setup")?;
    let sums = get(release.sums.as_ref().ok_or("The release has no checksums")?)?;
    let want = (sums.lines())
        .find_map(|l| {
            let (hash, file) = l.split_once(char::is_whitespace)?;
            (file.trim().trim_start_matches('*') == name).then(|| hash.to_ascii_lowercase())
        })
        .ok_or("The setup isn't in the release's checksums")?;
    let dir = std::env::temp_dir().join("vsn-update");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(name);
    let sha = |bytes: &mut dyn Read| -> std::io::Result<String> {
        let mut hasher = Sha256::new();
        std::io::copy(bytes, &mut hasher)?;
        Ok(format!("{:x}", hasher.finalize()))
    };
    if fs::File::open(&path).and_then(|mut f| sha(&mut f)).is_ok_and(|h| h == want) {
        return Ok(path);
    }
    let part = path.with_extension("part");
    let mut response = ureq::get(url)
        .header("User-Agent", "vsn-update")
        .call()
        .map_err(|e| e.to_string())?;
    let mut file = fs::File::create(&part).map_err(|e| e.to_string())?;
    let mut reader = response.body_mut().as_reader();
    let mut hasher = Sha256::new();
    let mut buf = vec![0; 1 << 16];
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
    }
    drop(file);
    if format!("{:x}", hasher.finalize()) != want {
        fs::remove_file(&part).ok();
        return Err("The downloaded setup doesn't match its checksum".into());
    }
    fs::rename(&part, &path).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Runs the setup Update Now asked for, once vsn has quit: silently, closing
/// what's left of vsn (the extension host's Node) through Restart Manager,
/// and with `/update=1`, which starts vsn again when it's done.
pub fn launch_pending() {
    let Some(setup) = PENDING.lock().ok().and_then(|mut p| p.take()) else {
        return;
    };
    let log = std::env::temp_dir().join("vsn-update").join("setup.log");
    std::process::Command::new(setup)
        .args([
            "/VERYSILENT",
            "/SUPPRESSMSGBOXES",
            "/NORESTART",
            "/CLOSEAPPLICATIONS",
            "/FORCECLOSEAPPLICATIONS",
            "/NORESTARTAPPLICATIONS",
            "/update=1",
        ])
        .arg(format!("/LOG={}", log.display()))
        .spawn()
        .ok();
}

type Found = Result<Option<(Release, Option<PathBuf>)>, String>;

impl Workbench {
    /// Checks soon after start, then every few hours, while `update.mode` is
    /// "default".
    pub fn start_update_checks(&mut self, cx: &mut Context<Self>) {
        self.update.timer = Some(cx.spawn(async move |this, cx| {
            let mut wait = FIRST_CHECK;
            loop {
                cx.background_executor().timer(wait).await;
                if (this.update(cx, |this, cx| this.check_for_updates(false, cx))).is_err() {
                    return;
                }
                wait = EVERY;
            }
        }));
    }

    /// Asks GitHub for the latest release and, for an installed vsn, downloads
    /// its setup. `manual` is Check for Updates...: it runs whatever the mode,
    /// and says so when there's nothing new or the check fails.
    pub(super) fn check_for_updates(&mut self, manual: bool, cx: &mut Context<Self>) {
        if !manual && mode() != "default" {
            return;
        }
        if self.update.checking {
            return;
        }
        self.update.checking = true;
        let installed = installed();
        cx.spawn(async move |this, cx| {
            let found: Found = cx
                .background_spawn(async move {
                    let release = latest()?;
                    if !newer(&release.version, env!("CARGO_PKG_VERSION")) {
                        return Ok(None);
                    }
                    let setup = match installed && release.setup.is_some() {
                        true => Some(download(&release)?),
                        false => None,
                    };
                    Ok(Some((release, setup)))
                })
                .await;
            this.update(cx, |this, cx| {
                this.update.checking = false;
                this.update_found(found, manual, cx);
            })
            .ok();
        })
        .detach();
    }

    fn update_found(&mut self, found: Found, manual: bool, cx: &mut Context<Self>) {
        let (release, setup) = match found {
            Ok(Some(found)) => found,
            Ok(None) if manual => {
                let none = "There are currently no updates available.";
                return self.note(none.into(), Severity::Info, cx);
            }
            Err(err) if manual => return self.error(format!("Unable to check for updates: {err}"), cx),
            // An automatic check that fails (offline) stays quiet, as VS Code's does.
            _ => return,
        };
        if !manual && self.update.offered.as_ref() == Some(&release.version) {
            return;
        }
        self.update.offered = Some(release.version.clone());
        let page = release.page;
        let notes: NoteAction = Rc::new(move |_, _, cx| cx.open_url(&page));
        let later: NoteAction = Rc::new(|_, _, _| {});
        match setup {
            Some(setup) => self.note_with(
                format!(
                    "Restart Visual Studio Code Native to apply the latest update ({}).",
                    release.version
                ),
                Severity::Info,
                vec![
                    ("Update Now".into(), Rc::new(move |this, window, cx| this.update_now(setup.clone(), window, cx))),
                    ("Later".into(), later),
                    ("Release Notes".into(), notes),
                ],
                cx,
            ),
            None => self.note_with(
                format!(
                    "A new version of Visual Studio Code Native is available ({}).",
                    release.version
                ),
                Severity::Info,
                vec![("Download".into(), notes), ("Later".into(), later)],
                cx,
            ),
        }
    }

    /// Quits as File > Exit does (asking about unsaved files), and the setup
    /// runs once vsn has. Cancelling leaves it to run on the next quit, as
    /// VS Code's update does.
    fn update_now(&mut self, setup: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if let Ok(mut pending) = PENDING.lock() {
            *pending = Some(setup);
        }
        self.quit(&Quit, window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[std::prelude::v1::test]
    fn versions() {
        assert_eq!(version("v0.1.2-1.95.0"), Some((0, 1, 2)));
        assert_eq!(version("0.1"), Some((0, 1, 0)));
        assert_eq!(version("vx"), None);
        assert!(newer("0.1.2", "0.1.1"));
        assert!(newer("0.2.0", "0.1.9"));
        assert!(!newer("0.1.1", "0.1.1"));
        assert!(!newer("0.1.0", "0.1.1"));
        assert!(!newer("junk", "0.1.1"));
    }

    /// The live release: found, and its setup downloads and matches its
    /// checksum. Network, so only on `cargo test -- --ignored`.
    #[std::prelude::v1::test]
    #[ignore]
    fn live_release() {
        let release = latest().unwrap();
        assert!(version(&release.version).is_some(), "{}", release.version);
        assert!(release.page.contains("/releases/tag/"), "{}", release.page);
        let setup = download(&release).unwrap();
        assert!(fs::metadata(&setup).unwrap().len() > 1_000_000);
        // Downloaded again, the kept copy is reused.
        assert_eq!(download(&release).unwrap(), setup);
    }
}
