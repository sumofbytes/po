//! Self-update from GitHub releases (`po update`).
//!
//! `po update` downloads the latest released build for this platform and
//! replaces the running binary in place. Like [`crate::dex`]'s own client it
//! has no async crate of its own for this, so the flow is plain blocking code
//! that runs in `main` before the GUI is set up.
//!
//! The flow mirrors `scripts/install.sh`: resolve latest via the
//! `/releases/latest` redirect (no API call, so no rate limits), fetch
//! `po-vX.Y.Z-<target>.tar.gz` + `SHA256SUMS`, verify sha256, then atomically
//! rename the verified binary over the running exe (POSIX allows renaming
//! over a running image; the old inode stays alive for in-flight processes).
//! `PO_BASE_URL` points the whole flow at a different host — used by the
//! release E2E test, and the same escape hatch install.sh has.
use std::{env, fs, io, path::Path, path::PathBuf, process::Command, time::Duration};

use ureq::ResponseExt;

/// Release version of this build — po is a single crate, so its own package
/// version is the released one.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Release host for the whole flow: `<BASE_URL>/releases/latest` redirects to
/// the tag page and `<BASE_URL>/releases/download/<tag>/<asset>` holds the
/// archives, like GitHub's. Defaults to GitHub; the E2E test points it at a
/// local stub server.
fn base_url() -> String {
    env::var("PO_BASE_URL")
        .ok()
        .map(|b| b.trim().trim_end_matches('/').to_string())
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| "https://github.com".to_string())
}

/// Progress line for a long network/IO phase, so a multi-MB download never
/// reads as a hung command, and the last one ends up in a useful place: the
/// final result goes to stdout, so `po update` stays script-friendly.
fn progress(message: &str) {
    eprintln!("{message}");
}

/// Entry point for `po update`.
pub fn update() -> Result<String, String> {
    let exe = env::current_exe()
        .map_err(|e| format!("cannot locate the running binary: {e}"))?
        .canonicalize()
        .map_err(|e| format!("cannot locate the running binary: {e}"))?;
    check_install_source(&exe)?;
    let target = release_target()?;

    // PO_VERSION (same env as install.sh) pins a version: it installs that
    // release even when it is older, i.e. it can downgrade/force-reinstall.
    let pinned = env::var("PO_VERSION")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty() && v != "latest");

    let (tag, latest) = match &pinned {
        Some(v) => {
            let tag = pinned_tag(v);
            let version = parse_version(&tag)
                .ok_or_else(|| format!("PO_VERSION '{v}' is not a vX.Y.Z version"))?;
            (tag, version)
        }
        None => {
            // Follow the /releases/latest redirect; the tag is the last path
            // segment of the URL we landed on.
            progress("checking the latest release ...");
            let resp = agent(Duration::from_secs(30))
                .get(format!("{}/releases/latest", base_url()))
                .call()
                .map_err(|e| format!("cannot check the latest release: {e}"))?;
            let tag = resp
                .get_uri()
                // The tag is the last path segment of the URL the redirect landed on.
                .to_string()
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string();
            let version = parse_version(&tag)
                .ok_or_else(|| format!("no valid vX.Y.Z release found (got '{tag}')"))?;
            (tag, version)
        }
    };

    let current = parse_version(VERSION).expect("po release version is semver");
    if pinned.is_none() && latest <= current {
        return Ok(format!("po {VERSION} is up to date (latest release {tag})"));
    }

    // Download, verify, extract, install. The temp dir is removed on every
    // path; the staged copy in the install dir is removed on rename failure.
    let dir = temp_download_dir()?;
    let outcome = download_verify_and_install(&exe, &tag, target, &dir);
    let _ = fs::remove_dir_all(&dir);
    outcome.map(|installed| format!("po {VERSION} -> {tag} — updated at {installed}"))
}

/// Download `po-{tag}-{target}.tar.gz` + `SHA256SUMS`, verify the archive's
/// sha256, extract with the system tar and replace the running binary.
fn download_verify_and_install(
    exe: &Path,
    tag: &str,
    target: &str,
    dir: &Path,
) -> Result<String, String> {
    let base = format!("{}/releases/download/{tag}", base_url());
    let name = format!("po-{tag}-{target}");
    let asset = format!("{name}.tar.gz");
    let archive = dir.join(&asset);

    // A generous cap for the whole exchange (connect + a multi-MB GUI
    // tarball): a hung server should fail the update, not park it forever.
    let client = agent(Duration::from_secs(600));
    progress(&format!("downloading {asset} ..."));
    fetch_file(&client, &format!("{base}/{asset}"), &archive)?;
    let sums = client
        .get(format!("{base}/SHA256SUMS"))
        .call()
        .and_then(|resp| resp.into_body().read_to_string())
        .map_err(|e| format!("download failed ({base}/SHA256SUMS): {e}"))?;

    let expected = checksum_for(&sums, &asset)
        .ok_or_else(|| format!("no SHA256SUMS entry for {asset}"))?;
    let actual = sha256_of(&archive).ok_or_else(|| {
        "neither sha256sum nor shasum is available to verify the download".to_string()
    })?;
    if actual != expected {
        return Err(format!(
            "checksum mismatch for {asset} (got {actual}, want {expected})"
        ));
    }

    // System tar, the same assumption install.sh already makes; avoids adding
    // flate2 + tar crates just for this. The release archives hold the binary
    // under `po-<tag>-<target>/po`, like install.sh unpacks them.
    progress("installing ...");
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(dir)
        .status()
        .map_err(|e| format!("cannot run tar: {e}"))?;
    if !status.success() {
        return Err("tar extraction failed".to_string());
    }
    let new_bin = dir.join(&name).join("po");
    if !new_bin.is_file() {
        return Err(format!("archive did not contain a po binary ({asset})"));
    }
    install(&new_bin, exe)
}

/// An HTTP client with the one timeout work it is given: short for the small
/// requests, long for the tarball.
fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(timeout)).build().into()
}

/// Download a URL to a file, so a tens-of-MB tarball never sits in memory.
fn fetch_file(client: &ureq::Agent, url: &str, to: &Path) -> Result<(), String> {
    let resp = client
        .get(url)
        .call()
        .map_err(|e| format!("download failed ({url}): {e}"))?;
    let mut reader = resp.into_body().into_reader();
    let mut out = fs::File::create(to).map_err(|e| format!("cannot write {}: {e}", to.display()))?;
    io::copy(&mut reader, &mut out)
        .map_err(|e| format!("download failed ({url}): {e}"))?;
    Ok(())
}

/// Move the verified binary into place: copy to a uniquely named staged file
/// next to the exe, chmod 755, rename over the running binary.
fn install(new_bin: &Path, exe: &Path) -> Result<String, String> {
    let dir = exe
        .parent()
        .ok_or_else(|| "binary has no parent directory".to_string())?;
    let staged = dir.join(format!(".po.new-{}", std::process::id()));
    let _ = fs::remove_file(&staged);
    fs::copy(new_bin, &staged).map_err(|e| {
        format!(
            "cannot stage the new binary in {} (is the install directory writable?): {e}",
            dir.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&staged)
            .map_err(|e| format!("cannot stat {}: {e}", staged.display()))?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&staged, perms)
            .map_err(|e| format!("cannot chmod {}: {e}", staged.display()))?;
    }
    fs::rename(&staged, exe).map_err(|e| {
        let _ = fs::remove_file(&staged);
        format!("cannot replace {}: {e}", exe.display())
    })?;
    Ok(exe.display().to_string())
}

/// Refuse to touch binaries we do not manage: replacing a dev build from a
/// release would silently lie about its version, and replacing a
/// cargo-installed binary would leave `~/.cargo/.crates.toml` stale.
fn check_install_source(exe: &Path) -> Result<(), String> {
    if cfg!(target_os = "windows") {
        return Err(
            "self-update is not supported on Windows — download a release from \
             https://github.com/sumofbytes/po/releases"
                .to_string(),
        );
    }
    if exe.components().any(|c| c.as_os_str() == "target") {
        return Err(
            "this is a dev build (running from target/) — update via git pull + cargo build"
                .to_string(),
        );
    }
    let cargo_home = env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")));
    if let Some(cargo_home) = cargo_home
        && exe.starts_with(cargo_home.join("bin"))
        && let Ok(text) = fs::read_to_string(cargo_home.join(".crates.toml"))
        && crates_toml_tracks(text.as_str())
    {
        return Err("installed via cargo — update with `cargo install`".to_string());
    }
    Ok(())
}

/// Does `~/.cargo/.crates.toml` (v1 or v2 format) track a package named po?
fn crates_toml_tracks(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim();
        line == "name = \"po\"" || line.starts_with("\"po ")
    })
}

/// Release target triple for this build, matching `release.yml`'s matrix.
fn release_target() -> Result<&'static str, String> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Ok("x86_64-unknown-linux-gnu")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Ok("aarch64-apple-darwin")
    } else {
        Err(
            "self-update is not available for this platform — build from source or download a \
             release from https://github.com/sumofbytes/po/releases"
                .to_string(),
        )
    }
}

/// Normalize a pinned `PO_VERSION` value into a release tag: `v`/`V` prefixes
/// and bare versions all become `vX.Y.Z` (prefix matched case-insensitively so
/// `PO_VERSION=V0.2.0` works too).
fn pinned_tag(v: &str) -> String {
    format!("v{}", v.trim().trim_start_matches(['v', 'V']))
}

/// Parse `vX.Y.Z` (or bare `X.Y.Z`, prefix case-insensitive) into a
/// comparable tuple; pre-release tags do not parse (we only publish plain
/// vX.Y.Z).
fn parse_version(tag: &str) -> Option<(u64, u64, u64)> {
    let version = tag.trim().trim_start_matches(['v', 'V']);
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Expected sha256 for `asset` in a `sha256sum`-format file (`<hash>  <name>`).
fn checksum_for(sums: &str, asset: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let hash = fields.next()?;
        let name = fields.next()?;
        (name == asset).then(|| hash.to_string())
    })
}

/// Digest a file the way install.sh does: `sha256sum` where it exists (Linux),
/// else `shasum -a 256` (macOS). None when neither tool runs successfully.
fn sha256_of(path: &Path) -> Option<String> {
    for mut command in [
        Command::new("sha256sum"),
        {
            let mut shasum = Command::new("shasum");
            shasum.arg("-a").arg("256");
            shasum
        },
    ] {
        let Ok(output) = command.arg(path).output() else { continue };
        if !output.status.success() {
            continue;
        }
        if let Some(hash) = String::from_utf8_lossy(&output.stdout).split_whitespace().next() {
            return Some(hash.to_string());
        }
    }
    None
}

/// Fresh unique temp dir for this run: a shared name would let two concurrent
/// `po update` processes extract into each other's files.
fn temp_download_dir() -> Result<PathBuf, String> {
    let dir = env::temp_dir().join(format!("po-update-{}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create temp dir {}: {e}", dir.display()))?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_compares_versions() {
        assert_eq!(parse_version("v0.2.1"), Some((0, 2, 1)));
        assert_eq!(parse_version("V1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("v1.0.0-rc1"), None);
        assert_eq!(parse_version("nope"), None);
        assert!(parse_version("v0.2.2") > parse_version("v0.2.1"));
        assert_eq!(pinned_tag("v0.2.0"), "v0.2.0");
        assert_eq!(pinned_tag("V0.2.0"), "v0.2.0");
        assert_eq!(pinned_tag(" 0.2.0 "), "v0.2.0");
    }

    #[test]
    fn checksum_parsing_matches_sha256sum_format() {
        let sums = "abc123  po-v0.2.1-x86_64-unknown-linux-gnu.tar.gz\n\
                    def456  po-v0.2.1-aarch64-apple-darwin.tar.gz\n";
        assert_eq!(
            checksum_for(sums, "po-v0.2.1-x86_64-unknown-linux-gnu.tar.gz").as_deref(),
            Some("abc123")
        );
        assert_eq!(checksum_for(sums, "missing.tar.gz"), None);
    }

    #[test]
    fn this_build_has_a_supported_release_target() {
        // Running the tests from target/-built test binaries: the release
        // targets are exactly the ones release.yml builds.
        release_target().expect("this build's platform must be supported");
    }

    #[test]
    fn archive_inner_paths_match_release_yml() {
        let tag = "v0.2.1";
        let target = "x86_64-unknown-linux-gnu";
        assert_eq!(
            format!("po-{tag}-{target}.tar.gz"),
            "po-v0.2.1-x86_64-unknown-linux-gnu.tar.gz"
        );
        assert_eq!(format!("po-{tag}-{target}/po"), "po-v0.2.1-x86_64-unknown-linux-gnu/po");
    }

    #[test]
    fn crates_toml_v1_and_v2_are_recognized() {
        assert!(crates_toml_tracks(
            "[[package]]\nname = \"po\"\nversion = \"0.2.1\"\n"
        ));
        assert!(crates_toml_tracks(
            "[v2]\n\"po 0.2.1 (registry+https://github.com/rust-lang/crates.io-index)\" = [\"po\"]\n"
        ));
        // " optimal " and "policy" contain "po" — substring matching must not fire.
        assert!(!crates_toml_tracks("name = \"optimal\"\n"));
        assert!(!crates_toml_tracks(""));
    }
}
