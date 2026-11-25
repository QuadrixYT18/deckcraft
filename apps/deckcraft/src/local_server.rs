//! The live-session server that ships with the app (`deckcraft-server`, next to the app's own
//! executable). Review › Live Session starts it when nothing answers on this computer, so hosting
//! needs no terminal. It runs on after the app quits, so people in a session aren't cut off;
//! `server/stop.command` (or the Activity Monitor) stops it. Data, log and PID file are where the
//! start scripts put them, so the scripts and the app share one server.

use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How long the server may take to answer after it was started.
const START_TIMEOUT: Duration = Duration::from_secs(20);

fn data_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        return Some(PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support/DeckCraft/live-server"));
    }
    if cfg!(windows) {
        return Some(PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("DeckCraft").join("live-server"));
    }
    let base =
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("deckcraft/live-server"))
}

/// The server program and the browser editor's files beside this executable: in the macOS app
/// bundle (`Contents/MacOS`, `Contents/Resources/web`) or a build tree (`target/release`, `dist/web`).
fn locate() -> Result<(PathBuf, Option<PathBuf>), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("no program folder")?;
    let server = dir.join(format!("deckcraft-server{}", std::env::consts::EXE_SUFFIX));
    if !server.is_file() {
        return Err(format!("{} is missing; start the server with server/start.command instead", server.display()));
    }
    let web = [dir.join("../Resources/web"), dir.join("web"), dir.join("../../dist/web")].into_iter().find(|w| w.join("index.html").is_file());
    Ok((server, web))
}

fn answers(port: u16) -> bool {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(2))).build().into();
    agent.get(&format!("http://127.0.0.1:{port}/healthz")).call().is_ok()
}

/// Make sure a server answers on `port` of this computer, starting the bundled one if needed.
pub fn ensure(port: u16) -> Result<(), String> {
    if answers(port) {
        return Ok(());
    }
    let (server, web) = locate()?;
    let data = data_dir().ok_or("no folder for the server's data (HOME is not set)")?;
    std::fs::create_dir_all(&data).map_err(|e| format!("can't create {}: {e}", data.display()))?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(data.join("server.log")).map_err(|e| e.to_string())?;
    let err = log.try_clone().map_err(|e| e.to_string())?;
    let mut cmd = std::process::Command::new(&server);
    cmd.env("DECKCRAFT_DATA", &data).env("DECKCRAFT_BIND", format!("0.0.0.0:{port}")).stdin(std::process::Stdio::null()).stdout(log).stderr(err);
    if let Some(web) = web {
        cmd.env("DECKCRAFT_WEB_DIR", web);
    }
    let child = cmd.spawn().map_err(|e| format!("{}: {e}", server.display()))?;
    // The start and stop scripts find it by this file.
    let _ = std::fs::write(data.join("server.pid"), child.id().to_string());
    log::info!("live session server started (process {}), data in {}", child.id(), data.display());
    let start = Instant::now();
    while start.elapsed() < START_TIMEOUT {
        if answers(port) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(format!("it started but doesn't answer; see {}", data.join("server.log").display()))
}
