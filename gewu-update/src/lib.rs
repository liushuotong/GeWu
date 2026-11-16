//! Release checks and self-update for the `gewu` app.
//!
//! The app is notification-only: [`check`] reports whether a newer release
//! exists, and [`install`] is the explicit `gewu update` download-and-replace
//! path. Checks are cached under `~/.gewu/update-check` for a day so normal
//! starts do not repeatedly hit GitHub.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use semver::Version;
use sha2::{Digest, Sha256};
use tracing::debug;

/// GitHub repository that publishes GeWu releases.
pub const REPO: &str = "regulatory-genomics/GeWu";
/// How long a release check stays fresh in the local cache.
pub const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

const CACHE_FILE: &str = "update-check";
const CHECK_TIMEOUT: Duration = Duration::from_secs(10);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(180);
const USER_AGENT: &str = concat!("gewu-update/", env!("CARGO_PKG_VERSION"));

/// A newer release of the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Update {
    pub current: Version,
    pub latest: Version,
    pub tag: String,
}

/// Whether automatic release checks are enabled.
pub fn enabled() -> bool {
    match std::env::var("GEWU_NO_UPDATE_CHECK") {
        Ok(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => true,
    }
}

/// Repository to check, overridable for forks and tests.
pub fn repo() -> String {
    std::env::var("GEWU_REPO")
        .ok()
        .map(|repo| repo.trim().to_string())
        .filter(|repo| !repo.is_empty())
        .unwrap_or_else(|| REPO.to_string())
}

/// Parse a release tag such as `v0.1.5`.
pub fn parse_tag(tag: &str) -> Result<Version> {
    let raw = tag.strip_prefix('v').unwrap_or(tag);
    Version::parse(raw).with_context(|| format!("invalid release tag {tag:?}"))
}

/// Return an [`Update`] when `tag` is newer than `current`.
pub fn update_from_tags(current: &str, tag: &str) -> Result<Option<Update>> {
    let current = Version::parse(current).context("invalid running version")?;
    let latest = parse_tag(tag)?;
    Ok((latest > current).then(|| Update {
        current,
        latest,
        tag: tag.to_string(),
    }))
}

/// One-line notice shared by the terminal and the GUI.
pub fn notice(update: &Update) -> String {
    format!(
        "Update available: gewu v{} → v{}. Run `gewu update` to install.",
        update.current, update.latest
    )
}

/// Release target triple for an OS/architecture pair.
pub fn target_for(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        _ => None,
    }
}

/// Release target of the running build, when one is published.
pub fn current_target() -> Option<&'static str> {
    // The Linux release is glibc-only; a musl build cannot replace itself
    // with the published binary.
    if cfg!(all(target_os = "linux", not(target_env = "gnu"))) {
        return None;
    }
    target_for(std::env::consts::OS, std::env::consts::ARCH)
}

/// Release archive name for a tag and target triple.
pub fn asset_name(tag: &str, target: &str) -> String {
    format!("gewu-{tag}-{target}.tar.gz")
}

/// Find the checksum for `asset` in a `SHA256SUMS` file.
pub fn checksum_for(sums: &str, asset: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let hash = fields.next()?;
        let name = fields.next()?.trim_start_matches('*');
        if name == asset && hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
            Some(hash.to_ascii_lowercase())
        } else {
            None
        }
    })
}

/// Read the newest available update from the local cache, without network.
pub fn cached(current: &str) -> Option<Update> {
    let path = cache_path()?;
    let entry = read_cache_from(&path)?;
    update_from_tags(current, &entry.tag).ok().flatten()
}

/// Check for an update, using a fresh cached result when possible.
pub async fn check(current: &str, force: bool) -> Result<Option<Update>> {
    if !force
        && let Some(entry) = cache_path().and_then(|path| read_cache_from(&path))
        && cache_is_fresh(&entry)
    {
        return update_from_tags(current, &entry.tag);
    }

    let client = http_client(CHECK_TIMEOUT)?;
    let tag = fetch_latest_tag(&client).await?;
    let update = update_from_tags(current, &tag)?;
    if let Some(path) = cache_path() {
        let _ = write_cache_to(&path, &tag);
    }
    Ok(update)
}

/// Print an update notice when one is available. Startup checks are best
/// effort and never interrupt the app.
pub async fn notify(current: &str) {
    if !enabled() {
        return;
    }
    match check(current, false).await {
        Ok(Some(update)) => eprintln!("{}", notice(&update)),
        Ok(None) => {}
        Err(e) => debug!("update check failed: {e:#}"),
    }
}

/// `gewu update` implementation. With `check_only`, only report what is
/// available; with `force`, reinstall even when the running version is newer.
pub async fn install(current: &str, check_only: bool, force: bool) -> Result<()> {
    let current_version = Version::parse(current).context("invalid running version")?;
    let exe = executable_path()?;
    if !check_only && !force && is_dev_build(&exe) {
        bail!(
            "refusing to replace the development build at {}; \
             run `gewu update` from an installed binary or pass --force",
            exe.display()
        );
    }

    let client = http_client(DOWNLOAD_TIMEOUT)?;
    let tag = fetch_latest_tag(&client).await?;
    let latest = parse_tag(&tag)?;
    if let Some(path) = cache_path() {
        let _ = write_cache_to(&path, &tag);
    }

    if !force && latest <= current_version {
        if check_only {
            println!("gewu v{current_version} is up to date (latest: v{latest}).");
        } else {
            println!("gewu v{current_version} is already up to date.");
        }
        return Ok(());
    }

    if check_only {
        println!(
            "Update available: gewu v{current_version} → v{latest}. Run `gewu update` to install."
        );
        return Ok(());
    }

    let target = current_target().ok_or_else(|| {
        anyhow!(
            "no prebuilt gewu release for {}/{}; build from source with `cargo install --path gewu`",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;

    let asset = asset_name(&tag, target);
    let repo = repo();
    let base = format!("https://github.com/{repo}/releases/download/{tag}");
    println!("Downloading gewu v{latest} ({target})...");

    let temp_dir = tempfile::tempdir().context("could not create a temporary directory")?;
    let sums_path = temp_dir.path().join("SHA256SUMS");
    let archive_path = temp_dir.path().join(&asset);
    download_to(&client, &format!("{base}/SHA256SUMS"), &sums_path).await?;
    download_to(&client, &format!("{base}/{asset}"), &archive_path).await?;

    let sums = fs::read_to_string(&sums_path).context("could not read SHA256SUMS")?;
    let expected = checksum_for(&sums, &asset)
        .ok_or_else(|| anyhow!("no checksum for {asset} in SHA256SUMS"))?;
    let actual = sha256_file(&archive_path)?;
    if actual != expected {
        bail!("checksum mismatch for {asset}: expected {expected}, got {actual}");
    }

    let temp_exe = unique_temp_path(&exe)?;
    let result: Result<()> = async {
        extract_gewu(&archive_path, &temp_exe)?;
        smoke_test(&temp_exe, &latest)?;
        replace_executable(&temp_exe, &exe)?;
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_file(&temp_exe);
    }
    result?;

    println!("Updated gewu to v{latest}. Restart gewu to use the new version.");
    Ok(())
}

fn http_client(timeout: Duration) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(timeout)
        .build()
        .context("could not create the HTTP client")
}

async fn fetch_latest_tag(client: &reqwest::Client) -> Result<String> {
    let repo = repo();
    let url = format!("https://github.com/{repo}/releases/latest");
    let response = client
        .head(&url)
        .send()
        .await
        .with_context(|| format!("could not reach {url}"))?;
    let status = response.status();
    let final_url = response.url().clone();
    if status == reqwest::StatusCode::NOT_FOUND {
        bail!("no published release found for {repo}");
    }
    if !status.is_success() {
        bail!("could not resolve the latest release of {repo}: HTTP {status}");
    }
    let tag = final_url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .unwrap_or_default();
    if tag.is_empty() || tag == "latest" {
        bail!("could not determine the latest release tag from {final_url}");
    }
    parse_tag(tag)?;
    Ok(tag.to_string())
}

async fn download_to(client: &reqwest::Client, url: &str, path: &Path) -> Result<()> {
    let mut response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("download failed: {url}"))?
        .error_for_status()
        .with_context(|| format!("download failed: {url}"))?;
    let mut file =
        File::create(path).with_context(|| format!("could not create {}", path.display()))?;
    while let Some(chunk) = response.chunk().await? {
        file.write_all(&chunk)
            .with_context(|| format!("could not write {}", path.display()))?;
    }
    file.flush()?;
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("could not open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn extract_gewu(archive: &Path, dest: &Path) -> Result<()> {
    let file =
        File::open(archive).with_context(|| format!("could not open {}", archive.display()))?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    for entry in archive
        .entries()
        .context("could not read the release archive")?
    {
        let mut entry = entry.context("could not read a release archive entry")?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .context("could not read an archive entry name")?;
        if path.as_ref() != Path::new("gewu") {
            continue;
        }
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dest)
            .with_context(|| {
                format!(
                    "could not create {}; check that its directory is writable",
                    dest.display()
                )
            })?;
        std::io::copy(&mut entry, &mut output)
            .with_context(|| format!("could not write {}", dest.display()))?;
        output.flush()?;
        set_executable(dest)?;
        return Ok(());
    }
    bail!("release archive does not contain a gewu binary");
}

fn smoke_test(path: &Path, expected: &Version) -> Result<()> {
    let output = Command::new(path)
        .arg("--version")
        .output()
        .with_context(|| format!("could not run {}", path.display()))?;
    if !output.status.success() {
        bail!(
            "the downloaded gewu failed to start: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let reported = String::from_utf8_lossy(&output.stdout);
    if !reported.contains(&expected.to_string()) {
        bail!("the downloaded gewu reported {reported:?}, expected v{expected}");
    }
    Ok(())
}

fn executable_path() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("could not locate the running gewu binary")?;
    Ok(fs::canonicalize(&exe).unwrap_or(exe))
}

fn unique_temp_path(exe: &Path) -> Result<PathBuf> {
    let dir = exe.parent().unwrap_or_else(|| Path::new("."));
    let name = exe
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("gewu");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    Ok(dir.join(format!(".{name}.update-{}-{nanos}", std::process::id())))
}

fn replace_executable(new: &Path, exe: &Path) -> Result<()> {
    fs::rename(new, exe).with_context(|| {
        format!(
            "could not replace {}; check its permissions or rerun with sudo",
            exe.display()
        )
    })
}

fn is_dev_build(path: &Path) -> bool {
    let parts: Vec<String> = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts
        .windows(2)
        .any(|pair| pair[0] == "target" && (pair[1] == "debug" || pair[1] == "release"))
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .with_context(|| format!("could not make {} executable", path.display()))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

#[derive(Debug)]
struct CacheEntry {
    checked_at: u64,
    tag: String,
}

fn cache_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".gewu").join(CACHE_FILE))
}

fn read_cache_from(path: &Path) -> Option<CacheEntry> {
    let text = fs::read_to_string(path).ok()?;
    let (checked_at, tag) = text.split_once(' ')?;
    Some(CacheEntry {
        checked_at: checked_at.trim().parse().ok()?,
        tag: tag.trim().to_string(),
    })
}

fn write_cache_to(path: &Path, tag: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    fs::write(path, format!("{} {tag}\n", now_secs()))
        .with_context(|| format!("could not write {}", path.display()))
}

fn cache_is_fresh(entry: &CacheEntry) -> bool {
    let now = now_secs();
    entry.checked_at <= now && now - entry.checked_at < CACHE_TTL.as_secs()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;

    const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn parses_tags_and_orders_versions() {
        assert_eq!(parse_tag("v1.2.3").unwrap(), Version::new(1, 2, 3));
        assert!(parse_tag("main").is_err());
        assert_eq!(
            update_from_tags("1.2.2", "v1.2.3").unwrap().unwrap().latest,
            Version::new(1, 2, 3)
        );
        assert!(update_from_tags("1.2.3", "v1.2.3").unwrap().is_none());
        assert!(update_from_tags("1.2.4", "v1.2.3").unwrap().is_none());
    }

    #[test]
    fn maps_supported_platforms_to_release_targets() {
        assert_eq!(
            target_for("linux", "x86_64"),
            Some("x86_64-unknown-linux-gnu")
        );
        assert_eq!(target_for("macos", "aarch64"), Some("aarch64-apple-darwin"));
        assert_eq!(target_for("windows", "x86_64"), None);
        assert_eq!(target_for("linux", "aarch64"), None);
        assert_eq!(
            asset_name("v1.2.3", "x86_64-unknown-linux-gnu"),
            "gewu-v1.2.3-x86_64-unknown-linux-gnu.tar.gz"
        );
    }

    #[test]
    fn finds_checksums_with_or_without_binary_markers() {
        let sums = format!("{HASH}  gewu-a.tar.gz\n{HASH} *gewu-b.tar.gz\n");
        assert_eq!(checksum_for(&sums, "gewu-a.tar.gz").as_deref(), Some(HASH));
        assert_eq!(checksum_for(&sums, "gewu-b.tar.gz").as_deref(), Some(HASH));
        assert!(checksum_for(&sums, "gewu-c.tar.gz").is_none());
        assert!(checksum_for("bad  gewu-a.tar.gz", "gewu-a.tar.gz").is_none());
    }

    #[test]
    fn extracts_only_the_gewu_binary() {
        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("release.tar.gz");
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut builder = tar::Builder::new(&mut encoder);
            append(&mut builder, "README.md", b"readme");
            append(&mut builder, "gewu", b"binary");
            builder.finish().unwrap();
        }
        fs::write(&archive_path, encoder.finish().unwrap()).unwrap();

        let dest = dir.path().join("gewu.new");
        extract_gewu(&archive_path, &dest).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"binary");

        let empty_path = dir.path().join("empty.tar.gz");
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut builder = tar::Builder::new(&mut encoder);
            append(&mut builder, "README.md", b"readme");
            builder.finish().unwrap();
        }
        fs::write(&empty_path, encoder.finish().unwrap()).unwrap();
        assert!(extract_gewu(&empty_path, &dir.path().join("missing")).is_err());
    }

    fn append(builder: &mut tar::Builder<&mut GzEncoder<Vec<u8>>>, name: &str, data: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, name, data).unwrap();
    }

    #[test]
    fn detects_development_builds() {
        assert!(is_dev_build(Path::new("/repo/target/debug/gewu")));
        assert!(is_dev_build(Path::new("/repo/target/release/gewu")));
        assert!(!is_dev_build(Path::new("/home/user/.local/bin/gewu")));
    }

    #[test]
    fn cache_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".gewu").join(CACHE_FILE);
        write_cache_to(&path, "v1.2.3").unwrap();
        let entry = read_cache_from(&path).unwrap();
        assert_eq!(entry.tag, "v1.2.3");
        assert!(cache_is_fresh(&entry));
    }

    #[cfg(unix)]
    #[test]
    fn replaces_the_target_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("gewu");
        let new = dir.path().join("gewu.new");
        fs::write(&target, b"old").unwrap();
        fs::write(&new, b"new").unwrap();
        replace_executable(&new, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert!(!new.exists());
    }
}
