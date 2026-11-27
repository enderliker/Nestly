//! Audio source resolution for `YouTube` and `SoundCloud`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use songbird::input::metadata::YoutubeDlOutput;
use songbird::input::{Compose, HlsRequest, HttpRequest, Input, LiveInput};
use tokio::process::Command;
use tokio::sync::{OnceCell, Semaphore};

static YTDLP_PROGRAM: OnceCell<String> = OnceCell::const_new();
static YTDLP_SEMAPHORE: Semaphore = Semaphore::const_new(1);

const YTDLP_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_YTDLP_NICE: &str = "19";

#[cfg(target_os = "linux")]
const YTDLP_ZIPAPP_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp";
#[cfg(target_os = "linux")]
const YTDLP_ONEDIR_URL: &str =
    "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_linux.zip";
#[cfg(not(target_os = "linux"))]
const YTDLP_ZIPAPP_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp";
#[cfg(not(target_os = "linux"))]
const YTDLP_ONEDIR_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp";

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && (meta.permissions().mode() & 0o111 != 0))
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file())
}

fn resolve_executable(program: &str) -> Option<String> {
    if program.contains(std::path::MAIN_SEPARATOR) || program.contains('/') {
        let path = Path::new(program);
        return is_executable_file(path).then(|| program.to_owned());
    }
    which(program).map(|path| path.to_string_lossy().into_owned())
}

fn cache_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(state_dir) = std::env::var("NESTLY_STATE_DIR") {
        let trimmed = state_dir.trim();
        if !trimmed.is_empty() {
            dirs.push(PathBuf::from(trimmed).join("yt-dlp"));
        }
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        dirs.push(dir.join("yt-dlp-dir"));
    }
    dirs.push(std::env::temp_dir().join("nestly-yt-dlp-dir"));
    dirs
}

fn prefer_onedir() -> bool {
    if let Ok(mode) = std::env::var("NESTLY_YTDLP_MODE") {
        let trimmed = mode.trim();
        if trimmed.eq_ignore_ascii_case("zipapp") {
            return false;
        }
        if trimmed.eq_ignore_ascii_case("onedir") {
            return true;
        }
    }
    cfg!(all(target_os = "linux", target_arch = "x86_64"))
}

fn find_cached_binary(dirs: &[PathBuf], rel_paths: &[&str]) -> Option<String> {
    for dir in dirs {
        for rel in rel_paths {
            let candidate = dir.join(rel);
            if is_executable_file(&candidate) {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }
    }
    None
}

async fn download_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|err| format!("failed to download yt-dlp from {url}: {err}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "failed to download yt-dlp from {url}: HTTP {}",
            response.status()
        ));
    }
    response
        .bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|err| format!("failed to read yt-dlp download: {err}"))
}

async fn install_zipapp(client: &reqwest::Client, dir: &Path) -> Result<PathBuf, String> {
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|err| format!("failed to create {}: {err}", dir.display()))?;
    let dest = dir.join("yt-dlp");
    tracing::info!(path = %dest.display(), "downloading yt-dlp zipapp");
    let bytes = download_bytes(client, YTDLP_ZIPAPP_URL).await?;
    let tmp = dest.with_extension("tmp");
    tokio::fs::write(&tmp, &bytes)
        .await
        .map_err(|err| format!("failed to write {}: {err}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .await
            .map_err(|err| format!("failed to chmod {}: {err}", tmp.display()))?;
    }
    tokio::fs::rename(&tmp, &dest).await.map_err(|err| {
        format!(
            "failed to move {} to {}: {err}",
            tmp.display(),
            dest.display()
        )
    })?;
    Ok(dest)
}

fn extract_onedir_zip(bytes: &[u8], dir: &Path) -> Result<PathBuf, String> {
    let staging = dir.join("onedir.tmp");
    let final_dir = dir.join("onedir");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .map_err(|err| format!("failed to create {}: {err}", staging.display()))?;

    let res = extract_onedir_into(&staging, bytes);
    let rel_bin = match res {
        Ok(rel) => rel,
        Err(err) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(err);
        }
    };

    let backup = dir.join("onedir.old");
    let _ = std::fs::remove_dir_all(&backup);
    if final_dir.exists() {
        std::fs::rename(&final_dir, &backup).map_err(|err| {
            let _ = std::fs::remove_dir_all(&staging);
            format!("failed to replace {}: {err}", final_dir.display())
        })?;
    }
    if let Err(err) = std::fs::rename(&staging, &final_dir) {
        if backup.exists() {
            let _ = std::fs::rename(&backup, &final_dir);
        }
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!(
            "failed to move {} to {}: {err}",
            staging.display(),
            final_dir.display()
        ));
    }
    let _ = std::fs::remove_dir_all(&backup);

    let bin = final_dir.join(rel_bin);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755));
    }
    Ok(bin)
}

fn extract_onedir_into(staging: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
    let cursor = std::io::Cursor::new(bytes);
    let mut archive =
        zip::ZipArchive::new(cursor).map_err(|err| format!("invalid yt-dlp zip archive: {err}"))?;
    let mut rel_binary: Option<PathBuf> = None;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|err| format!("zip entry error: {err}"))?;
        let Some(rel_path) = entry.enclosed_name() else {
            continue;
        };
        let out_path = staging.join(&rel_path);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)
                .map_err(|err| format!("failed to create {}: {err}", out_path.display()))?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("failed to create {}: {err}", parent.display()))?;
        }
        #[cfg(unix)]
        if entry.is_symlink() {
            use std::io::Read;
            let mut target = String::new();
            entry
                .read_to_string(&mut target)
                .map_err(|err| format!("failed to read symlink {}: {err}", out_path.display()))?;
            let target_path = Path::new(&target);
            let safe = !target_path.is_absolute()
                && !target_path
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir));
            if safe {
                std::os::unix::fs::symlink(target_path, &out_path).map_err(|err| {
                    format!("failed to create symlink {}: {err}", out_path.display())
                })?;
                continue;
            }
        }
        let mut outfile = std::fs::File::create(&out_path)
            .map_err(|err| format!("failed to create {}: {err}", out_path.display()))?;
        std::io::copy(&mut entry, &mut outfile)
            .map_err(|err| format!("failed to extract {}: {err}", out_path.display()))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Some(mode) = entry.unix_mode() {
                let _ = std::fs::set_permissions(&out_path, std::fs::Permissions::from_mode(mode));
            }
        }

        if rel_path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| name == "yt-dlp_linux" || name == "yt-dlp")
        {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&out_path, std::fs::Permissions::from_mode(0o755));
            }
            rel_binary = Some(rel_path);
        }
    }

    rel_binary.ok_or_else(|| "yt-dlp executable not found inside zip archive".to_owned())
}

async fn install_onedir(client: &reqwest::Client, dir: &Path) -> Result<PathBuf, String> {
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|err| format!("failed to create {}: {err}", dir.display()))?;
    tracing::info!(dir = %dir.display(), "downloading yt-dlp onedir archive");
    let bytes = download_bytes(client, YTDLP_ONEDIR_URL).await?;
    let dir_owned = dir.to_path_buf();
    tokio::task::spawn_blocking(move || extract_onedir_zip(&bytes, &dir_owned))
        .await
        .map_err(|err| format!("yt-dlp extraction task failed: {err}"))?
}

/// Resolves a working `yt-dlp` binary path without spawning `--version` probes.
///
/// On `x86_64` Linux, prefers the pre-extracted `onedir` bundle over the compressed
/// Python zipapp so every invocation skips zip decompression and bytecode compilation.
pub async fn resolve_ytdlp_program(client: &reqwest::Client) -> Result<&'static str, String> {
    let resolved = YTDLP_PROGRAM
        .get_or_try_init(|| async {
            let program = resolve_installed_ytdlp(client).await?;
            if let Some((nice, level)) = nice_launcher() {
                tracing::info!(
                    program = %program,
                    nice = %nice.display(),
                    level = %level,
                    "yt-dlp configured at low CPU priority"
                );
            } else {
                tracing::info!(program = %program, "yt-dlp configured");
            }
            Ok::<String, String>(program)
        })
        .await?;
    Ok(resolved.as_str())
}

async fn resolve_installed_ytdlp(client: &reqwest::Client) -> Result<String, String> {
    if let Ok(custom) = std::env::var("YTDLP_PATH") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            return resolve_executable(trimmed)
                .ok_or_else(|| format!("YTDLP_PATH '{trimmed}' is not executable"));
        }
    }

    for name in ["yt-dlp_linux", "yt-dlp"] {
        if let Some(found) = resolve_executable(name) {
            return Ok(found);
        }
    }

    let dirs = cache_dirs();
    if let Some(cached) = find_cached_binary(
        &dirs,
        &["onedir/yt-dlp_linux", "onedir/yt-dlp", "yt-dlp_linux"],
    ) {
        return Ok(cached);
    }

    let mut last_err = String::from("no writable path for yt-dlp");
    if prefer_onedir() {
        for dir in &dirs {
            match install_onedir(client, dir).await {
                Ok(bin) if is_executable_file(&bin) => {
                    return Ok(bin.to_string_lossy().into_owned());
                }
                Ok(bin) => {
                    last_err = format!("installed yt-dlp at {} is not executable", bin.display());
                }
                Err(err) => last_err = err,
            }
        }
        if let Some(cached_zipapp) = find_cached_binary(&dirs, &["yt-dlp"]) {
            tracing::warn!(
                error = %last_err,
                fallback = %cached_zipapp,
                "could not install yt-dlp onedir bundle; falling back to cached zipapp"
            );
            return Ok(cached_zipapp);
        }
    } else if let Some(cached_zipapp) = find_cached_binary(&dirs, &["yt-dlp"]) {
        return Ok(cached_zipapp);
    }

    if !cfg!(target_os = "linux") || resolve_executable("python3").is_some() {
        for dir in &dirs {
            match install_zipapp(client, dir).await {
                Ok(bin) if is_executable_file(&bin) => {
                    return Ok(bin.to_string_lossy().into_owned());
                }
                Ok(bin) => {
                    last_err = format!("installed yt-dlp at {} is not executable", bin.display());
                }
                Err(err) => last_err = err,
            }
        }
    }

    Err(last_err)
}

#[cfg(unix)]
fn nice_launcher() -> Option<(&'static Path, String)> {
    nice_launcher_from(std::env::var("NESTLY_YTDLP_NICE").ok().as_deref())
}

#[cfg(unix)]
fn nice_launcher_from(override_level: Option<&str>) -> Option<(&'static Path, String)> {
    static NICE_BIN: OnceLock<Option<PathBuf>> = OnceLock::new();

    let raw = override_level.unwrap_or(DEFAULT_YTDLP_NICE).trim();
    if raw.is_empty() || raw == "0" || raw.eq_ignore_ascii_case("off") {
        return None;
    }
    let nice = NICE_BIN.get_or_init(|| which("nice")).as_deref()?;
    Some((nice, raw.to_owned()))
}

#[cfg(not(unix))]
fn nice_launcher() -> Option<(&'static Path, String)> {
    None
}

fn build_ytdlp_command(ytdlp_program: &str, target: &str) -> Command {
    let mut cmd = if let Some((nice, level)) = nice_launcher() {
        let mut c = Command::new(nice);
        c.arg("-n").arg(level).arg(ytdlp_program);
        c
    } else {
        Command::new(ytdlp_program)
    };
    cmd.stdin(Stdio::null());
    cmd.kill_on_drop(true);
    cmd.args([
        "-S",
        "acodec:opus",
        "-j",
        "-f",
        "ba[abr>0][vcodec=none]/best",
        "--no-playlist",
        "--no-warnings",
        "--",
        target,
    ]);
    cmd
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable_file(candidate))
}

/// Supported providers for music search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AudioProvider {
    #[default]
    Youtube,
    Soundcloud,
}

impl AudioProvider {
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "soundcloud" | "sc" => Self::Soundcloud,
            _ => Self::Youtube,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Youtube => "YouTube",
            Self::Soundcloud => "SoundCloud",
        }
    }
}

/// Metadata extracted from a resolved track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackMetadata {
    pub title: String,
    pub artist: String,
    pub duration_secs: Option<u64>,
    pub source_url: Option<String>,
}

/// Resolves a user input query or URL into a `yt-dlp` target.
#[must_use]
pub fn resolve_query(query: &str, provider: AudioProvider) -> String {
    let trimmed = query.trim();
    if is_url(trimmed) {
        trimmed.to_owned()
    } else {
        match provider {
            AudioProvider::Youtube => format!("ytsearch:{trimmed}"),
            AudioProvider::Soundcloud => format!("scsearch:{trimmed}"),
        }
    }
}

/// Checks whether an input string is an HTTP or HTTPS URL.
#[must_use]
pub fn is_url(input: &str) -> bool {
    input.starts_with("http://") || input.starts_with("https://")
}

/// A track resolved by `yt-dlp`: its metadata plus a ready-to-play input.
pub struct ResolvedTrack {
    pub metadata: TrackMetadata,
    pub input: Input,
}

/// Resolves a search query or URL into metadata and a playable input.
///
/// This is the only place that spawns `yt-dlp`, and it spawns it exactly once.
/// `yt-dlp` extraction is the expensive part of starting a track (a full Python
/// process that has to solve `YouTube`'s player challenges), so the resulting
/// stream URL is handed to `Songbird` directly. Letting `Songbird` own a
/// `YoutubeDl` input instead would re-run the whole extraction as soon as
/// playback begins.
pub async fn resolve_track(
    ytdlp_program: &'static str,
    client: reqwest::Client,
    target: String,
) -> Result<ResolvedTrack, String> {
    let _permit = YTDLP_SEMAPHORE
        .acquire()
        .await
        .map_err(|err| format!("yt-dlp semaphore closed: {err}"))?;

    let mut cmd = build_ytdlp_command(ytdlp_program, &target);
    let output = tokio::time::timeout(YTDLP_TIMEOUT, cmd.output())
        .await
        .map_err(|_| format!("yt-dlp timed out after {}s", YTDLP_TIMEOUT.as_secs()))?
        .map_err(|err| format!("cannot run {ytdlp_program}: {err}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        return Err(if detail.is_empty() {
            format!("{ytdlp_program} failed with {}", output.status)
        } else {
            format!("{ytdlp_program} failed with {}: {detail}", output.status)
        });
    }

    let mut results = output
        .stdout
        .split(|&b| b == b'\n')
        .filter(|line| !line.is_empty())
        .map(serde_json::from_slice::<YoutubeDlOutput>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| format!("invalid yt-dlp metadata JSON: {err}"))?;

    if results.is_empty() {
        return Err("yt-dlp returned no playable result".to_owned());
    }
    let result = results.swap_remove(0);

    let metadata = metadata_from(&result);
    let input = stream_input(client, &result)?;
    Ok(ResolvedTrack { metadata, input })
}

fn metadata_from(result: &YoutubeDlOutput) -> TrackMetadata {
    let aux = result.as_aux_metadata();
    TrackMetadata {
        title: aux
            .title
            .or(aux.track)
            .unwrap_or_else(|| "Unknown title".to_owned()),
        artist: aux
            .artist
            .or(aux.channel)
            .unwrap_or_else(|| "Unknown artist".to_owned()),
        duration_secs: aux.duration.map(|d| d.as_secs()),
        source_url: aux.source_url,
    }
}

/// Builds the playback input from the URL `yt-dlp` already resolved.
fn stream_input(client: reqwest::Client, result: &YoutubeDlOutput) -> Result<Input, String> {
    let headers = http_headers(result);

    if result.protocol.as_deref() == Some("m3u8_native") {
        let mut request = HlsRequest::new_with_headers(client, result.url.clone(), headers);
        let stream = request
            .create()
            .map_err(|err| format!("cannot open the HLS stream: {err}"))?;
        return Ok(Input::Live(LiveInput::Raw(stream), None));
    }

    Ok(HttpRequest {
        client,
        request: result.url.clone(),
        headers,
        content_length: result.filesize,
    }
    .into())
}

fn http_headers(result: &YoutubeDlOutput) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Some(map) = &result.http_headers {
        for (name, value) in map {
            let (Ok(name), Ok(value)) = (
                HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_str(value),
            ) else {
                continue;
            };
            headers.insert(name, value);
        }
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::{AudioProvider, is_url, resolve_query};

    /// A stub `yt-dlp` that records its own command line and answers with one JSON
    /// result, so the whole resolution path can be exercised without a network.
    #[cfg(unix)]
    fn stub_ytdlp(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;

        let log = dir.join("calls.log");
        let program = dir.join("yt-dlp");
        let json = r#"{"url":"https://example.invalid/audio.webm","title":"Test Track","artist":"Tester","duration":123.0,"filesize":12345,"protocol":"https","http_headers":{"User-Agent":"test-agent"}}"#;
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nprintf '%s\\n' '{}'\n",
            log.display(),
            json
        );
        std::fs::write(&program, script).expect("write stub");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub");
        (program, log)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn resolve_track_spawns_yt_dlp_exactly_once() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (program, log) = stub_ytdlp(dir.path());
        let program: &'static str =
            Box::leak(program.to_string_lossy().into_owned().into_boxed_str());

        let resolved = super::resolve_track(
            program,
            reqwest::Client::new(),
            "https://example.invalid/watch".to_owned(),
        )
        .await
        .expect("resolution should succeed");

        assert_eq!(resolved.metadata.title, "Test Track");
        assert_eq!(resolved.metadata.artist, "Tester");
        assert_eq!(resolved.metadata.duration_secs, Some(123));
        assert!(
            matches!(resolved.input, songbird::input::Input::Lazy(_)),
            "playback must reuse the resolved URL, not re-run yt-dlp"
        );

        let calls = std::fs::read_to_string(&log).expect("read call log");
        let calls: Vec<&str> = calls.lines().collect();
        assert_eq!(
            calls.len(),
            1,
            "yt-dlp must run exactly once per track, got {calls:?}"
        );
        assert!(calls[0].contains("-j"), "expected a metadata extraction");
        assert!(
            calls[0].contains("acodec:opus"),
            "must prefer the Opus stream for passthrough playback: {}",
            calls[0]
        );
    }

    #[test]
    fn detects_urls() {
        assert!(is_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ"));
        assert!(is_url("http://soundcloud.com/artist/track"));
        assert!(!is_url("never gonna give you up"));
        assert!(!is_url("ytsearch:lofi hip hop"));
    }

    #[test]
    fn resolves_queries_by_provider() {
        assert_eq!(
            resolve_query("lofi beats", AudioProvider::Youtube),
            "ytsearch:lofi beats"
        );
        assert_eq!(
            resolve_query("synthwave", AudioProvider::Soundcloud),
            "scsearch:synthwave"
        );
        // Direct URLs remain unchanged regardless of provider
        assert_eq!(
            resolve_query("https://soundcloud.com/user/track", AudioProvider::Youtube),
            "https://soundcloud.com/user/track"
        );
        assert_eq!(
            resolve_query("https://youtu.be/dQw4w9WgXcQ", AudioProvider::Soundcloud),
            "https://youtu.be/dQw4w9WgXcQ"
        );
    }

    #[test]
    fn provider_from_str() {
        assert_eq!(AudioProvider::from_str("youtube"), AudioProvider::Youtube);
        assert_eq!(
            AudioProvider::from_str("soundcloud"),
            AudioProvider::Soundcloud
        );
        assert_eq!(AudioProvider::from_str("sc"), AudioProvider::Soundcloud);
        assert_eq!(AudioProvider::from_str("anything"), AudioProvider::Youtube);
    }

    #[cfg(unix)]
    #[test]
    fn executable_check_does_not_spawn_process() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("temp dir");
        let log = dir.path().join("invoked.log");
        let bin = dir.path().join("yt-dlp_linux");
        let script = format!("#!/bin/sh\necho invoked >> '{}'\n", log.display());
        std::fs::write(&bin, script).expect("write binary");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let found = super::find_cached_binary(
            &[dir.path().to_path_buf()],
            &["onedir/yt-dlp_linux", "yt-dlp_linux"],
        );
        assert_eq!(found.as_deref(), Some(bin.to_string_lossy().as_ref()));
        assert!(
            !log.exists(),
            "binary check must inspect filesystem permissions without executing the binary"
        );
    }

    #[cfg(unix)]
    #[test]
    fn nice_launcher_respects_disable_overrides() {
        assert!(super::nice_launcher_from(Some("0")).is_none());
        assert!(super::nice_launcher_from(Some("off")).is_none());
        assert!(super::nice_launcher_from(Some("OFF")).is_none());
        assert!(super::nice_launcher_from(Some("   ")).is_none());
        if let Some((_path, level)) = super::nice_launcher_from(None) {
            assert_eq!(level, super::DEFAULT_YTDLP_NICE);
        }
    }

    #[test]
    fn extract_onedir_zip_installs_atomically() {
        use std::io::Write;

        let mut buf = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default();
            writer.start_file("yt-dlp_linux", opts).unwrap();
            writer.write_all(b"#!/bin/sh\necho ok\n").unwrap();
            writer.start_file("_internal/libpython.so", opts).unwrap();
            writer.write_all(b"dummy").unwrap();
            writer.finish().unwrap();
        }

        let dir = tempfile::tempdir().expect("temp dir");
        let bin = super::extract_onedir_zip(&buf, dir.path()).expect("extract succeeds");
        assert_eq!(bin, dir.path().join("onedir/yt-dlp_linux"));
        assert!(bin.exists());
        assert!(dir.path().join("onedir/_internal/libpython.so").exists());
        assert!(!dir.path().join("onedir.tmp").exists());
        assert!(!dir.path().join("onedir.old").exists());
    }
}
