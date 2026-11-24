//! Guild player and track queue management.

use std::collections::{HashMap, VecDeque};
use std::num::NonZeroU64;

use rsbot_module_sdk::{VoiceServerPayload, VoiceStatePayload};
use songbird::driver::Driver;
use songbird::input::YoutubeDl;
use songbird::tracks::TrackHandle;
use songbird::{Config, ConnectionInfo};

/// A track waiting or playing in the guild queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedTrack {
    pub target: String,
    pub title: String,
    pub artist: String,
    pub duration_secs: Option<u64>,
    pub requester_id: Option<u64>,
}

/// Manages music playback for a single Discord server.
pub struct GuildPlayer {
    pub guild_id: u64,
    pub driver: Driver,
    pub queue: VecDeque<QueuedTrack>,
    pub current: Option<QueuedTrack>,
    pub handle: Option<TrackHandle>,
    pub is_connected: bool,
    pub channel_id: Option<u64>,
    pub session_id: Option<String>,
    pub endpoint: Option<String>,
    pub token: Option<String>,
    pub is_paused: bool,
}

impl GuildPlayer {
    #[must_use]
    pub fn new(guild_id: u64) -> Self {
        Self {
            guild_id,
            driver: Driver::new(Config::default()),
            queue: VecDeque::new(),
            current: None,
            handle: None,
            is_connected: false,
            channel_id: None,
            session_id: None,
            endpoint: None,
            token: None,
            is_paused: false,
        }
    }

    /// Attempts to connect to Discord's voice server if all credentials are present.
    pub fn connect_if_ready(&mut self, bot_user_id: u64) -> bool {
        let (Some(channel_id), Some(session_id), Some(endpoint), Some(token)) = (
            self.channel_id,
            &self.session_id,
            &self.endpoint,
            &self.token,
        ) else {
            return false;
        };

        let Some(channel_nz) = NonZeroU64::new(channel_id) else {
            return false;
        };
        let Some(guild_nz) = NonZeroU64::new(self.guild_id) else {
            return false;
        };
        let Some(user_nz) = NonZeroU64::new(bot_user_id) else {
            return false;
        };

        let info = ConnectionInfo {
            channel_id: channel_nz.into(),
            endpoint: endpoint.clone(),
            guild_id: guild_nz.into(),
            session_id: session_id.clone(),
            token: token.clone(),
            user_id: user_nz.into(),
        };

        self.driver.connect(info);
        self.is_connected = true;
        true
    }

    /// Enqueues a track. If nothing is currently playing, starts playback immediately.
    pub fn enqueue(&mut self, track: QueuedTrack, client: reqwest::Client) {
        if self.current.is_none() {
            self.play_track(track, client);
        } else {
            self.queue.push_back(track);
        }
    }

    /// Starts playing a track immediately on the driver.
    pub fn play_track(&mut self, track: QueuedTrack, client: reqwest::Client) {
        let source = YoutubeDl::new(client, track.target.clone());
        let input: songbird::input::Input = source.into();
        let handle = self.driver.play_input(input);
        self.handle = Some(handle);
        self.current = Some(track);
        self.is_paused = false;
    }

    /// Skips the current track and starts the next queued track if available.
    pub fn skip(&mut self, client: reqwest::Client) -> Option<QueuedTrack> {
        if let Some(handle) = self.handle.take() {
            let _ = handle.stop();
        }
        self.current = None;
        if let Some(next) = self.queue.pop_front() {
            self.play_track(next.clone(), client);
            Some(next)
        } else {
            None
        }
    }

    /// Pauses or resumes the currently playing track.
    ///
    /// Returns `Ok(true)` if paused, `Ok(false)` if resumed, or an error if nothing is playing.
    pub fn pause_or_resume(&mut self) -> Result<bool, &'static str> {
        let Some(handle) = &self.handle else {
            return Err("Nothing is currently playing.");
        };
        if self.is_paused {
            let _ = handle.play();
            self.is_paused = false;
            Ok(false)
        } else {
            let _ = handle.pause();
            self.is_paused = true;
            Ok(true)
        }
    }

    /// Stops playback, clears the queue, and resets driver state.
    pub fn stop(&mut self) {
        self.queue.clear();
        if let Some(handle) = self.handle.take() {
            let _ = handle.stop();
        }
        self.current = None;
        self.is_paused = false;
        self.driver.stop();
        self.is_connected = false;
        self.channel_id = None;
        self.session_id = None;
        self.endpoint = None;
        self.token = None;
    }

    #[must_use]
    pub fn now_playing(&self) -> Option<&QueuedTrack> {
        self.current.as_ref()
    }

    #[must_use]
    pub fn queue_list(&self) -> Vec<&QueuedTrack> {
        self.queue.iter().collect()
    }
}

/// Central manager coordinating voice states and per-guild players.
pub struct MusicManager {
    players: HashMap<u64, GuildPlayer>,
    user_voice_channels: HashMap<(u64, u64), u64>,
    client: reqwest::Client,
    pub bot_user_id: Option<u64>,
}

impl MusicManager {
    #[must_use]
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            players: HashMap::new(),
            user_voice_channels: HashMap::new(),
            client,
            bot_user_id: None,
        }
    }

    pub fn player_mut(&mut self, guild_id: u64) -> &mut GuildPlayer {
        self.players
            .entry(guild_id)
            .or_insert_with(|| GuildPlayer::new(guild_id))
    }

    pub fn player(&self, guild_id: u64) -> Option<&GuildPlayer> {
        self.players.get(&guild_id)
    }

    #[must_use]
    pub fn client(&self) -> reqwest::Client {
        self.client.clone()
    }

    /// Resolves which voice channel a user is currently in within a guild.
    #[must_use]
    pub fn user_voice_channel(&self, guild_id: u64, user_id: u64) -> Option<u64> {
        self.user_voice_channels.get(&(guild_id, user_id)).copied()
    }

    /// Processes incoming voice state updates from Discord gateway.
    pub fn handle_voice_state(&mut self, state: VoiceStatePayload) {
        let Some(guild_id) = state.guild_id else {
            return;
        };

        // If the update is for the bot itself
        if let Some(bot_id) = self.bot_user_id
            && state.user_id == bot_id
        {
            let player = self.player_mut(guild_id);
            player.channel_id = state.channel_id;
            player.session_id = Some(state.session_id);
            player.connect_if_ready(bot_id);
            return;
        }

        // Track user voice presence
        if let Some(channel_id) = state.channel_id {
            self.user_voice_channels
                .insert((guild_id, state.user_id), channel_id);
        } else {
            self.user_voice_channels.remove(&(guild_id, state.user_id));
        }
    }

    /// Processes incoming voice server updates from Discord gateway.
    pub fn handle_voice_server(&mut self, server: VoiceServerPayload) {
        let bot_id = self.bot_user_id;
        let player = self.player_mut(server.guild_id);
        player.endpoint = server.endpoint;
        player.token = Some(server.token);
        if let Some(bot_id) = bot_id {
            player.connect_if_ready(bot_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{GuildPlayer, QueuedTrack};

    #[tokio::test]
    async fn player_queue_and_skip() {
        let mut player = GuildPlayer::new(123);
        assert!(player.now_playing().is_none());
        assert!(player.queue_list().is_empty());

        let t1 = QueuedTrack {
            target: "https://example.com/1.mp3".to_owned(),
            title: "Track 1".to_owned(),
            artist: "Artist 1".to_owned(),
            duration_secs: Some(180),
            requester_id: Some(42),
        };
        let t2 = QueuedTrack {
            target: "https://example.com/2.mp3".to_owned(),
            title: "Track 2".to_owned(),
            artist: "Artist 2".to_owned(),
            duration_secs: Some(210),
            requester_id: Some(42),
        };

        let client = reqwest::Client::new();
        player.enqueue(t1.clone(), client.clone());
        assert_eq!(player.now_playing(), Some(&t1));
        assert!(player.queue_list().is_empty());

        player.enqueue(t2.clone(), client.clone());
        assert_eq!(player.now_playing(), Some(&t1));
        assert_eq!(player.queue_list().len(), 1);
        assert_eq!(player.queue_list()[0], &t2);

        // Skip to next track
        let next = player.skip(client.clone());
        assert_eq!(next, Some(t2.clone()));
        assert_eq!(player.now_playing(), Some(&t2));
        assert!(player.queue_list().is_empty());

        // Skip when queue is empty
        let empty = player.skip(client);
        assert_eq!(empty, None);
        assert!(player.now_playing().is_none());
    }

    #[tokio::test]
    async fn player_stop_resets_state() {
        let mut player = GuildPlayer::new(123);
        let client = reqwest::Client::new();
        let t1 = QueuedTrack {
            target: "https://example.com/1.mp3".to_owned(),
            title: "Track 1".to_owned(),
            artist: "Artist 1".to_owned(),
            duration_secs: Some(180),
            requester_id: None,
        };
        player.enqueue(t1, client);
        assert!(player.now_playing().is_some());

        player.stop();
        assert!(player.now_playing().is_none());
        assert!(player.queue_list().is_empty());
        assert!(!player.is_connected);
    }
}
