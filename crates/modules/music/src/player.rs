//! Guild player and track queue management.

use std::collections::{HashMap, VecDeque};
use std::num::NonZeroU64;

use nestly_module_sdk::{VoiceServerPayload, VoiceStatePayload};
use songbird::driver::Driver;
use songbird::events::{Event, EventContext, EventHandler as VoiceEventHandler, TrackEvent};
use songbird::input::Input;
use songbird::tracks::TrackHandle;
use songbird::{Config, ConnectionInfo};
use tokio::sync::mpsc;

struct TrackErrorLogger;

#[async_trait::async_trait]
impl VoiceEventHandler for TrackErrorLogger {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if let EventContext::Track(tracks) = ctx {
            for (state, handle) in *tracks {
                tracing::error!(
                    uuid = %handle.uuid(),
                    playing = ?state.playing,
                    position = ?state.position,
                    "songbird track playback error"
                );
            }
        }
        None
    }
}

struct TrackEndNotifier {
    guild_id: u64,
    tx: mpsc::UnboundedSender<(u64, TrackHandle)>,
}

#[async_trait::async_trait]
impl VoiceEventHandler for TrackEndNotifier {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if let EventContext::Track(tracks) = ctx {
            for (_state, handle) in *tracks {
                let _ = self.tx.send((self.guild_id, (*handle).clone()));
            }
        }
        Some(Event::Cancel)
    }
}

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
    /// Tracks already resolved to a playable input, oldest first.
    pub queue: VecDeque<(QueuedTrack, Input)>,
    pub current: Option<QueuedTrack>,
    pub handle: Option<TrackHandle>,
    pub is_connected: bool,
    pub channel_id: Option<u64>,
    pub session_id: Option<String>,
    pub endpoint: Option<String>,
    pub token: Option<String>,
    pub is_paused: bool,
    end_tx: Option<mpsc::UnboundedSender<(u64, TrackHandle)>>,
}

impl GuildPlayer {
    #[must_use]
    pub fn new(guild_id: u64) -> Self {
        Self::create(guild_id, None)
    }

    #[must_use]
    pub fn with_end_notifier(
        guild_id: u64,
        end_tx: mpsc::UnboundedSender<(u64, TrackHandle)>,
    ) -> Self {
        Self::create(guild_id, Some(end_tx))
    }

    fn create(guild_id: u64, end_tx: Option<mpsc::UnboundedSender<(u64, TrackHandle)>>) -> Self {
        // Each guild plays at most one track at volume 1.0, so soft-clipping can be
        // skipped to save CPU on non-passthrough tracks.
        let config = Config::default().use_softclip(false);
        Self {
            guild_id,
            driver: Driver::new(config),
            queue: VecDeque::new(),
            current: None,
            handle: None,
            is_connected: false,
            channel_id: None,
            session_id: None,
            endpoint: None,
            token: None,
            is_paused: false,
            end_tx,
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

    /// Enqueues an already-resolved track. If nothing is currently playing, starts playback
    /// immediately.
    pub fn enqueue(&mut self, track: QueuedTrack, input: Input) {
        if self.current.is_none() {
            self.play_track(track, input);
        } else {
            self.queue.push_back((track, input));
        }
    }

    /// Starts playing a resolved track immediately on the driver.
    pub fn play_track(&mut self, track: QueuedTrack, input: Input) {
        // `play_only_input` guarantees a single live track in the mixer so Opus
        // passthrough is never disabled by an overlapping finishing track.
        let handle = self.driver.play_only_input(input);
        let _ = handle.add_event(Event::Track(TrackEvent::Error), TrackErrorLogger);
        if let Some(end_tx) = &self.end_tx {
            let _ = handle.add_event(
                Event::Track(TrackEvent::End),
                TrackEndNotifier {
                    guild_id: self.guild_id,
                    tx: end_tx.clone(),
                },
            );
        }
        self.handle = Some(handle);
        self.current = Some(track);
        self.is_paused = false;
    }

    /// Handles a natural track completion and starts the next queued track, if any.
    ///
    /// Ignores stale completion events from tracks that were already skipped or stopped.
    pub fn handle_track_end(&mut self, ended_handle: &TrackHandle) -> Option<QueuedTrack> {
        let is_current = self
            .handle
            .as_ref()
            .is_some_and(|current| current.uuid() == ended_handle.uuid());
        if !is_current {
            return None;
        }
        self.handle = None;
        self.current = None;
        self.is_paused = false;
        let (next, input) = self.queue.pop_front()?;
        self.play_track(next.clone(), input);
        Some(next)
    }

    /// Skips the current track and starts the next queued track if available.
    pub fn skip(&mut self) -> Option<QueuedTrack> {
        if let Some(handle) = self.handle.take() {
            let _ = handle.stop();
        }
        self.current = None;
        let (next, input) = self.queue.pop_front()?;
        self.play_track(next.clone(), input);
        Some(next)
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
        self.queue.iter().map(|(track, _)| track).collect()
    }
}

/// Central manager coordinating voice states and per-guild players.
pub struct MusicManager {
    players: HashMap<u64, GuildPlayer>,
    user_voice_channels: HashMap<(u64, u64), u64>,
    client: reqwest::Client,
    pub bot_user_id: Option<u64>,
    end_tx: Option<mpsc::UnboundedSender<(u64, TrackHandle)>>,
}

impl MusicManager {
    #[must_use]
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            players: HashMap::new(),
            user_voice_channels: HashMap::new(),
            client,
            bot_user_id: None,
            end_tx: None,
        }
    }

    #[must_use]
    pub fn with_end_notifier(
        client: reqwest::Client,
        end_tx: mpsc::UnboundedSender<(u64, TrackHandle)>,
    ) -> Self {
        Self {
            players: HashMap::new(),
            user_voice_channels: HashMap::new(),
            client,
            bot_user_id: None,
            end_tx: Some(end_tx),
        }
    }

    pub fn player_mut(&mut self, guild_id: u64) -> &mut GuildPlayer {
        let end_tx = self.end_tx.clone();
        self.players
            .entry(guild_id)
            .or_insert_with(|| match end_tx {
                Some(tx) => GuildPlayer::with_end_notifier(guild_id, tx),
                None => GuildPlayer::new(guild_id),
            })
    }

    /// Advances playback when a track finishes naturally in a guild.
    pub fn handle_track_end(
        &mut self,
        guild_id: u64,
        ended_handle: &TrackHandle,
    ) -> Option<QueuedTrack> {
        self.players
            .get_mut(&guild_id)?
            .handle_track_end(ended_handle)
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

    /// A lazy input that is never opened: these tests only exercise queue bookkeeping.
    fn dummy_input(name: &str) -> songbird::input::Input {
        songbird::input::File::new(format!("/nonexistent/{name}")).into()
    }

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

        player.enqueue(t1.clone(), dummy_input("1.mp3"));
        assert_eq!(player.now_playing(), Some(&t1));
        assert!(player.queue_list().is_empty());

        player.enqueue(t2.clone(), dummy_input("2.mp3"));
        assert_eq!(player.now_playing(), Some(&t1));
        assert_eq!(player.queue_list().len(), 1);
        assert_eq!(player.queue_list()[0], &t2);

        // Skip to next track
        let next = player.skip();
        assert_eq!(next, Some(t2.clone()));
        assert_eq!(player.now_playing(), Some(&t2));
        assert!(player.queue_list().is_empty());

        // Skip when queue is empty
        let empty = player.skip();
        assert_eq!(empty, None);
        assert!(player.now_playing().is_none());
    }

    #[tokio::test]
    async fn player_stop_resets_state() {
        let mut player = GuildPlayer::new(123);
        let t1 = QueuedTrack {
            target: "https://example.com/1.mp3".to_owned(),
            title: "Track 1".to_owned(),
            artist: "Artist 1".to_owned(),
            duration_secs: Some(180),
            requester_id: None,
        };
        player.enqueue(t1, dummy_input("1.mp3"));
        assert!(player.now_playing().is_some());

        player.stop();
        assert!(player.now_playing().is_none());
        assert!(player.queue_list().is_empty());
        assert!(!player.is_connected);
    }

    #[tokio::test]
    async fn player_track_end_advances_queue_and_ignores_stale_handles() {
        let mut player = GuildPlayer::new(123);
        let t1 = QueuedTrack {
            target: "https://example.com/1.mp3".to_owned(),
            title: "Track 1".to_owned(),
            artist: "Artist 1".to_owned(),
            duration_secs: Some(180),
            requester_id: None,
        };
        let t2 = QueuedTrack {
            target: "https://example.com/2.mp3".to_owned(),
            title: "Track 2".to_owned(),
            artist: "Artist 2".to_owned(),
            duration_secs: Some(210),
            requester_id: None,
        };

        player.enqueue(t1, dummy_input("1.mp3"));
        let first_handle = player.handle.clone().expect("first handle");
        player.enqueue(t2.clone(), dummy_input("2.mp3"));

        // Skip moves to Track 2, so the stale end event for Track 1 must be ignored.
        assert_eq!(player.skip(), Some(t2.clone()));
        assert_eq!(player.handle_track_end(&first_handle), None);
        assert_eq!(player.now_playing(), Some(&t2));

        // When Track 2 itself ends, current clears cleanly.
        let second_handle = player.handle.clone().expect("second handle");
        assert_eq!(player.handle_track_end(&second_handle), None);
        assert!(player.now_playing().is_none());
    }
}
