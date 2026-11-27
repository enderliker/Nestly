//! Audio source resolution for `YouTube` and `SoundCloud`.

use std::path::{Path, PathBuf};

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use songbird::input::metadata::YoutubeDlOutput;
use songbird::input::{Compose, HlsRequest, HttpRequest, Input, LiveInput, YoutubeDl};
use tokio::sync::OnceCell;

static YTDLP_PROGRAM: OnceCell<String> = OnceCell::const_new();

#[cfg(target_os = "linux")]
const YTDLP_ZIPAPP_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp";
#[cfg(target_os = "linux")]
const YTDLP_ONEDIR_URL: &str =
    "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_linux.zip";
#[cfg(not(target_os = "linux"))]
const YTDLP_ZIPAPP_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp";
#[cfg(not(target_os = "linux"))]
const YTDLP_ONEDIR_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp";

async fn is_executable(program: &str) -> bool {
    tokio::process::Command::new(program)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success())
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
    let cursor = std::io::Cursor::new(bytes);
    let mut archive =
        zip::ZipArchive::new(cursor).map_err(|err| format!("invalid yt-dlp zip archive: {err}"))?;
    let mut binary_path: Option<PathBuf> = None;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|err| format!("zip entry error: {err}"))?;
        let Some(rel_path) = entry.enclosed_name() else {
            continue;
        };
        let out_path = dir.join(rel_path);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)
                .map_err(|err| format!("failed to create {}: {err}", out_path.display()))?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("failed to create {}: {err}", parent.display()))?;
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

        if out_path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| name == "yt-dlp_linux" || name == "yt-dlp")
        {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&out_path, std::fs::Permissions::from_mode(0o755));
            }
            binary_path = Some(out_path);
        }
    }

    binary_path.ok_or_else(|| "yt-dlp executable not found inside zip archive".to_owned())
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

/// Resolves a working `yt-dlp` binary path, downloading the zipapp (if `python3` is
/// available) or the pre-extracted `onedir` bundle (no `/tmp` extraction) if missing.
///
/// The resolved program is wrapped in a `nice` launcher when one is available, so a
/// track extraction cannot starve the bot when the host is short on CPU.
pub async fn resolve_ytdlp_program(client: &reqwest::Client) -> Result<&'static str, String> {
    let resolved = YTDLP_PROGRAM
        .get_or_try_init(|| async {
            let program = resolve_installed_ytdlp(client).await?;
            if let Some(wrapper) = low_priority_wrapper(&program) {
                if is_executable(&wrapper).await {
                    tracing::info!(
                        program = %program,
                        wrapper = %wrapper,
                        "yt-dlp is launched at low CPU priority"
                    );
                    return Ok::<String, String>(wrapper);
                }
                tracing::warn!(
                    wrapper = %wrapper,
                    "the low-priority yt-dlp wrapper cannot run here; using yt-dlp directly"
                );
            }
            Ok(program)
        })
        .await?;
    Ok(resolved.as_str())
}

async fn resolve_installed_ytdlp(client: &reqwest::Client) -> Result<String, String> {
    if let Ok(custom) = std::env::var("YTDLP_PATH") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            if is_executable(trimmed).await {
                return Ok(trimmed.to_owned());
            }
            return Err(format!("YTDLP_PATH '{trimmed}' is not executable"));
        }
    }

    if is_executable("yt-dlp").await {
        return Ok("yt-dlp".to_owned());
    }

    let dirs = cache_dirs();
    for dir in &dirs {
        for name in ["yt-dlp_linux", "yt-dlp"] {
            let candidate = dir.join(name);
            let path_str = candidate.to_string_lossy();
            if candidate.exists() && is_executable(&path_str).await {
                return Ok(path_str.into_owned());
            }
        }
    }

    let has_python = is_executable("python3").await;
    let mut last_err = String::from("no writable path for yt-dlp");
    for dir in &dirs {
        let installed = if has_python {
            install_zipapp(client, dir).await
        } else if cfg!(target_os = "linux") {
            install_onedir(client, dir).await
        } else {
            install_zipapp(client, dir).await
        };
        match installed {
            Ok(bin) => {
                let path_str = bin.to_string_lossy().into_owned();
                if is_executable(&path_str).await {
                    return Ok(path_str);
                }
                last_err = format!("installed yt-dlp at {path_str} is not executable");
            }
            Err(err) => last_err = err,
        }
    }
    Err(last_err)
}

/// Writes a `nice` launcher for `program`, so `yt-dlp` runs at a lower CPU priority
/// than the bot itself.
///
/// `yt-dlp` is a full Python process whose `YouTube` extraction is CPU-bound; on a
/// container limited to one or two cores it otherwise delays gateway heartbeats and
/// slash-command replies. `NESTLY_YTDLP_NICE` overrides the adjustment (an empty
/// value, `0` or `off` disables the wrapper). Returns `None` whenever the wrapper
/// cannot be created, so callers fall back to running `program` directly.
#[cfg(unix)]
fn low_priority_wrapper(program: &str) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;

    let level = std::env::var("NESTLY_YTDLP_NICE").unwrap_or_else(|_| "10".to_owned());
    let level = level.trim();
    if level.is_empty() || level == "0" || level.eq_ignore_ascii_case("off") {
        return None;
    }
    let nice = which("nice")?;

    let dir = runtime_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let wrapper = dir.join("yt-dlp-nice");
    let quoted = program.replace('\'', r"'\''");
    let script = format!(
        "#!/bin/sh\n# generated by nestly: run yt-dlp at low CPU priority\nexec {} -n {} '{}' \"$@\"\n",
        nice.display(),
        level,
        quoted
    );
    if std::fs::read_to_string(&wrapper).ok().as_deref() != Some(script.as_str()) {
        std::fs::write(&wrapper, script).ok()?;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).ok()?;
    }
    Some(wrapper.to_string_lossy().into_owned())
}

/// No-op on platforms without `nice`.
#[cfg(not(unix))]
fn low_priority_wrapper(_program: &str) -> Option<String> {
    None
}

#[cfg(unix)]
fn runtime_dir() -> PathBuf {
    if let Ok(state_dir) = std::env::var("NESTLY_STATE_DIR") {
        let trimmed = state_dir.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed).join("runtime");
        }
    }
    std::env::temp_dir().join("nestly-runtime")
}

#[cfg(unix)]
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
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
    // `-S acodec:opus` prefers the WebM/Opus stream: 48 kHz with 20 ms frames, which
    // Songbird forwards to Discord verbatim ("passthrough") instead of decoding,
    // resampling and re-encoding it. It is a preference, so platforms without Opus
    // still resolve.
    let mut source = YoutubeDl::new_ytdl_like(ytdlp_program, client.clone(), target)
        .user_args(vec!["-S".to_owned(), "acodec:opus".to_owned()]);

    let mut results = source
        .query(1)
        .await
        .map_err(|err| format!("cannot resolve the track: {err}"))?;
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
}
