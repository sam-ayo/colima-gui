//! Finds new versions of the app, and installs them.
//!
//! Each release publishes `latest.json` and one installer package for each
//! architecture (see `scripts/release.sh`). The manifest gives an Ed25519
//! signature of each package. The signed message is the version, a newline,
//! and the package, so that an old package cannot show as a new version.
//! The app installs a package only when the signature agrees with
//! `PUBLIC_KEY`.
//!
//! The app looks for a new version at start and then at an interval. The
//! "Check for Updates…" menu item also starts a check. The installer needs an
//! administrator password, because the package links the tools into
//! `/usr/local/bin`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::{Signature, VerifyingKey};
use gpui::{App, AsyncApp, Global};
use serde::Deserialize;

use crate::runtime::Backend;
use crate::ui::native::{self, APP_NAME};

const MANIFEST_URL: &str =
    "https://github.com/sam-ayo/colima-gui-releases/releases/latest/download/latest.json";

/// An environment variable that replaces `MANIFEST_URL`, to test a release
/// from a local server. The packages must still have a correct signature.
const MANIFEST_URL_VAR: &str = "COLIMA_GUI_UPDATE_URL";

/// The public half of the release signing key, as base64 of the 32 raw
/// bytes. `scripts/release.sh` tells how to get it from the private key.
const PUBLIC_KEY: &str = "05Kb7PWs2zma3JsGydJxl8wMoC+wWZdQL5JUUzsCZoc=";

/// The installer package always puts the app here.
const APP_PATH: &str = "/Applications/Colima GUI.app";

/// The first check waits a short time, so that it does not slow the start.
const FIRST_CHECK: Duration = Duration::from_secs(10);
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// The key in the user defaults of the version that the user skipped.
const SKIPPED_KEY: &str = "SkippedUpdateVersion";

#[derive(Deserialize)]
struct Manifest {
    version: String,
    #[serde(default)]
    notes: String,
    /// The packages, by `std::env::consts::ARCH`.
    packages: HashMap<String, Package>,
}

#[derive(Deserialize)]
struct Package {
    url: String,
    signature: String,
}

/// True while a check or an install runs. A second check does not start
/// then.
struct Busy(bool);

impl Global for Busy {}

/// Starts the automatic checks. The app looks for updates only when it
/// runs from the installed bundle, not from `cargo run`.
pub fn init(cx: &mut App) {
    cx.set_global(Busy(false));
    if !installed() {
        return;
    }
    cx.spawn(async move |cx| {
        cx.background_executor().timer(FIRST_CHECK).await;
        loop {
            let _ = cx.update(|cx| check(false, cx));
            cx.background_executor().timer(CHECK_INTERVAL).await;
        }
    })
    .detach();
}

/// Looks for a new version. When `interactive` is true, the user asked for
/// the check, so the app also tells when there is no update or when the
/// check fails.
pub fn check(interactive: bool, cx: &mut App) {
    if !installed() {
        if interactive {
            alert(
                "Updates are not available".into(),
                format!("Only the app in {APP_PATH} can install updates."),
                cx,
            );
        }
        return;
    }
    if cx.global::<Busy>().0 {
        return;
    }
    cx.global_mut::<Busy>().0 = true;
    let backend = Backend::global(cx);
    cx.spawn(async move |cx| {
        if let Err(err) = run(interactive, &backend, cx).await {
            tracing::warn!("update failed: {err:#}");
            if interactive {
                native::show_alert("The update failed", &format!("{err:#}"), &["OK"]);
            }
        }
        let _ = cx.update(|cx| cx.global_mut::<Busy>().0 = false);
    })
    .detach();
}

async fn run(interactive: bool, backend: &Backend, cx: &mut AsyncApp) -> Result<()> {
    let manifest = backend.spawn(fetch_manifest()).await??;
    let current = env!("CARGO_PKG_VERSION");
    if !is_newer(&manifest.version, current) {
        if interactive {
            native::show_alert(
                &format!("{APP_NAME} is up to date"),
                &format!("You have the latest version, {current}."),
                &["OK"],
            );
        }
        return Ok(());
    }
    if !interactive && native::saved_string(SKIPPED_KEY).as_deref() == Some(&manifest.version) {
        return Ok(());
    }
    let Manifest { version, notes, mut packages } = manifest;
    let package = packages
        .remove(std::env::consts::ARCH)
        .ok_or_else(|| anyhow!("version {version} has no package for this Mac"))?;

    let mut info = format!("{APP_NAME} {version} is available. You have {current}.");
    if !notes.trim().is_empty() {
        info.push_str("\n\n");
        info.push_str(notes.trim());
    }
    // An automatic check waits until the user uses the app. The alert then
    // does not take the focus from another app.
    while !native::is_active() {
        cx.background_executor().timer(Duration::from_secs(1)).await;
    }
    let buttons = ["Install and Relaunch", "Later", "Skip This Version"];
    match native::show_alert("A new version is available", &info, &buttons) {
        Some(0) => {}
        Some(2) => {
            native::save_string(SKIPPED_KEY, &version);
            return Ok(());
        }
        _ => return Ok(()),
    }

    let path = backend.spawn(download(package, version)).await??;
    let done = backend
        .spawn(async move { tokio::task::spawn_blocking(move || install(&path)).await })
        .await???;
    if done {
        relaunch()?;
        let _ = cx.update(|cx| cx.quit());
    }
    Ok(())
}

fn installed() -> bool {
    std::env::current_exe().is_ok_and(|exe| exe.starts_with(APP_PATH))
}

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("colima-gui/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .build()?)
}

async fn fetch_manifest() -> Result<Manifest> {
    let url = std::env::var(MANIFEST_URL_VAR).unwrap_or_else(|_| MANIFEST_URL.into());
    client()?
        .get(url)
        .timeout(Duration::from_secs(30))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
        .context("could not read the update manifest")
}

/// Downloads the package and checks its signature. Returns the path of the
/// package.
async fn download(package: Package, version: String) -> Result<PathBuf> {
    let bytes = client()?
        .get(&package.url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await
        .context("could not download the update")?;
    verify(&version, &bytes, &package.signature)?;
    // The name does not use the version, because the manifest has no
    // signature and the version can contain any text.
    let dir = std::env::temp_dir().join("colima-gui-update");
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join("colima-gui.pkg");
    tokio::fs::write(&path, &bytes).await?;
    Ok(path)
}

fn verify(version: &str, package: &[u8], signature: &str) -> Result<()> {
    let key: [u8; 32] = BASE64
        .decode(PUBLIC_KEY)?
        .try_into()
        .map_err(|_| anyhow!("the public key is not 32 bytes"))?;
    let key = VerifyingKey::from_bytes(&key)?;
    let signature: [u8; 64] = BASE64
        .decode(signature.trim())
        .ok()
        .and_then(|s| s.try_into().ok())
        .context("the update has a bad signature")?;
    let mut message = Vec::with_capacity(version.len() + 1 + package.len());
    message.extend_from_slice(version.as_bytes());
    message.push(b'\n');
    message.extend_from_slice(package);
    key.verify_strict(&message, &Signature::from_bytes(&signature))
        .context("the signature of the update does not agree with the release key")
}

/// Runs the installer with administrator rights. macOS asks for the
/// password. Returns false when the user cancels.
fn install(package: &Path) -> Result<bool> {
    let script = [
        "on run argv",
        "do shell script \"/usr/sbin/installer -pkg \" & quoted form of (item 1 of argv) & \
         \" -target /\" with prompt \"Colima GUI wants to install an update.\" \
         with administrator privileges",
        "end run",
    ];
    let mut cmd = Command::new("/usr/bin/osascript");
    for line in script {
        cmd.args(["-e", line]);
    }
    let output = cmd.arg(package).output().context("could not start the installer")?;
    let _ = std::fs::remove_file(package);
    if output.status.success() {
        return Ok(true);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    // AppleScript error -128 means that the user cancelled.
    if stderr.contains("(-128)") {
        return Ok(false);
    }
    bail!("the installer failed: {}", stderr.trim())
}

/// Opens the new app after this process stops.
fn relaunch() -> Result<()> {
    let pid = std::process::id();
    Command::new("/bin/sh")
        .arg("-c")
        .arg(format!(
            "while /bin/kill -0 {pid} 2>/dev/null; do sleep 0.2; done; /usr/bin/open '{APP_PATH}'"
        ))
        .spawn()
        .context("could not relaunch the app")?;
    Ok(())
}

fn alert(message: String, info: String, cx: &mut App) {
    // An alert runs its own event loop, which must not run while GPUI
    // updates the app.
    cx.spawn(async move |_| native::show_alert(&message, &info, &["OK"]))
        .detach();
}

/// True when `new` is a later version than `current`. Both have the form
/// `1.2.3`. A version with another form is never newer.
fn is_newer(new: &str, current: &str) -> bool {
    let parse = |v: &str| -> Option<Vec<u64>> {
        v.trim_start_matches('v').split('.').map(|n| n.parse().ok()).collect()
    };
    match (parse(new), parse(current)) {
        (Some(new), Some(current)) => new > current,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.99.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(!is_newer("0.2.0-beta", "0.1.0"));
    }

    /// OpenSSL made this signature of "0.2.0\npackage" with the release key,
    /// as `scripts/release.sh` does.
    const SIGNATURE: &str =
        "V5Lsa9+9f8sF/kcnzpOGwtzXlt/rlzdNnTeKVCG6wCRkjR0bxkxQy+1kkeU90GAwg2wrLZ+5G0pU2muEM3hqDQ==";

    #[test]
    fn accepts_a_release_signature() {
        verify("0.2.0", b"package", SIGNATURE).unwrap();
    }

    #[test]
    fn rejects_a_signature_for_another_version_or_package() {
        assert!(verify("0.3.0", b"package", SIGNATURE).is_err());
        assert!(verify("0.2.0", b"packagf", SIGNATURE).is_err());
    }

    #[test]
    fn rejects_a_bad_signature() {
        assert!(verify("0.2.0", b"package", &BASE64.encode([0u8; 64])).is_err());
        assert!(verify("0.2.0", b"package", "not base64").is_err());
    }
}
