//! Audio source resolution for `YouTube` and `SoundCloud`.

use songbird::input::YoutubeDl;

/// Supported providers for music search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AudioProvider {
    #[default]
    Youtube,
    Soundcloud,
}

impl AudioProvider {
    #[must_use]
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
pub fn create_source(client: reqwest::Client, target: String) -> YoutubeDl<'static> {
    YoutubeDl::new(client, target)
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
