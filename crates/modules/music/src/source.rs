//! Audio source resolution for `YouTube` and `SoundCloud`.

use std::path::{Path, PathBuf};

use songbird::input::YoutubeDl;
use tokio::sync::OnceCell;

static YTDLP_PROGRAM: OnceCell<String> = OnceCell::const_new();

#[cfg(target_os = "linux")]
const YTDLP_DOWNLOAD_URL: &str =
    "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_linux";
#[cfg(not(target_os = "linux"))]
const YTDLP_DOWNLOAD_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp";

async fn is_executable(program: &str) -> bool {
    tokio::process::Command::new(program)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success())
}

fn candidate_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        paths.push(dir.join("yt-dlp"));
    }
    paths.push(std::env::temp_dir().join("nestly-yt-dlp"));
    paths
}

async fn download_ytdlp(client: &reqwest::Client, dest: &Path) -> Result<(), String> {
    tracing::info!(path = %dest.display(), "downloading yt-dlp binary");
    let response = client
        .get(YTDLP_DOWNLOAD_URL)
        .send()
        .await
        .map_err(|err| format!("failed to download yt-dlp: {err}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "failed to download yt-dlp: HTTP {}",
            response.status()
        ));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|err| format!("failed to read yt-dlp download: {err}"))?;

    let tmp = dest.with_extension("tmp");
    tokio::fs::write(&tmp, &bytes)
        .await
        .map_err(|err| format!("failed to write {}: {err}", tmp.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        tokio::fs::set_permissions(&tmp, perms)
            .await
            .map_err(|err| format!("failed to chmod {}: {err}", tmp.display()))?;
    }

    tokio::fs::rename(&tmp, dest).await.map_err(|err| {
        format!(
            "failed to move {} to {}: {err}",
            tmp.display(),
            dest.display()
        )
    })?;
    Ok(())
}

/// Resolves a working `yt-dlp` binary path, downloading the standalone binary if missing.
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

            let candidates = candidate_paths();
            for candidate in &candidates {
                let path_str = candidate.to_string_lossy();
                if candidate.exists() && is_executable(&path_str).await {
                    return Ok(path_str.into_owned());
                }
            }

            let mut last_err = String::from("no writable path for yt-dlp");
            for candidate in &candidates {
                match download_ytdlp(client, candidate).await {
                    Ok(()) => {
                        let path_str = candidate.to_string_lossy().into_owned();
                        if is_executable(&path_str).await {
                            return Ok(path_str);
                        }
                        last_err = format!("downloaded yt-dlp at {path_str} is not executable");
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
