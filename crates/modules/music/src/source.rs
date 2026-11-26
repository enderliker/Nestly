//! Audio source resolution for `YouTube` and `SoundCloud`.

use std::path::{Path, PathBuf};

use songbird::input::YoutubeDl;
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
pub async fn resolve_ytdlp_program(client: &reqwest::Client) -> Result<&'static str, String> {
    let resolved = YTDLP_PROGRAM
        .get_or_try_init(|| async {
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
        })
        .await?;
    Ok(resolved.as_str())
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

/// Creates a `YoutubeDl` input from a resolved query or direct URL.
#[must_use]
pub fn create_source(
    ytdlp_program: &'static str,
    client: reqwest::Client,
    target: String,
) -> YoutubeDl<'static> {
    YoutubeDl::new_ytdl_like(ytdlp_program, client, target)
}

/// Fetches metadata from a `YoutubeDl` source without playing it yet.
pub async fn fetch_metadata(source: &mut YoutubeDl<'static>) -> Result<TrackMetadata, String> {
    use songbird::input::Compose;
    let aux = source
        .aux_metadata()
        .await
        .map_err(|err| format!("cannot load track metadata: {err}"))?;

    let title = aux
        .title
        .or(aux.track)
        .unwrap_or_else(|| "Unknown title".to_owned());
    let artist = aux
        .artist
        .or(aux.channel)
        .unwrap_or_else(|| "Unknown artist".to_owned());
    let duration_secs = aux.duration.map(|d| d.as_secs());
    let source_url = aux.source_url;

    Ok(TrackMetadata {
        title,
        artist,
        duration_secs,
        source_url,
    })
}

#[cfg(test)]
mod tests {
    use super::{AudioProvider, is_url, resolve_query};

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
