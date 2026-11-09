//! Meetings: rows in `history.db`, audio in `<app_data>/meetings/<id>/`.
//! Every change to a meeting is announced as a [`MeetingUpdate`].

use anyhow::{anyhow, Result};
use log::{debug, error, info, warn};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager};
use tauri_specta::Event;

use crate::managers::history::HistoryManager;
use crate::settings::MeetingAudioRetention;

use super::capture::{MICROPHONE_TRACK, SYSTEM_TRACK};

/// The meetings table. Created outside the history migrations, like the Bin
/// tables: a higher `user_version` would make older builds refuse the
/// database, while an extra table is invisible to them.
pub const MEETINGS_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS meetings (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        title TEXT NOT NULL DEFAULT '',
        started_at INTEGER NOT NULL,
        duration_ms INTEGER NOT NULL DEFAULT 0,
        status TEXT NOT NULL,
        error TEXT,
        model_id TEXT,
        language TEXT,
        has_microphone BOOLEAN NOT NULL DEFAULT 0,
        has_system_audio BOOLEAN NOT NULL DEFAULT 0,
        audio TEXT NOT NULL DEFAULT 'kept',
        audio_binned_at INTEGER,
        binned_at INTEGER,
        recovered BOOLEAN NOT NULL DEFAULT 0,
        speaker_names TEXT NOT NULL DEFAULT '{}',
        segments TEXT NOT NULL DEFAULT '[]',
        transcribed_at INTEGER
    );";

/// Both tracks mixed, made on demand for playback and export.
pub const PLAYBACK_FILE: &str = "playback.wav";

/// Every column a [`Meeting`] is read from, its segments included.
const COLUMNS: &str = "id, title, started_at, duration_ms, status, error, model_id, language, \
    has_microphone, has_system_audio, audio, recovered, speaker_names, segments";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingStatus {
    /// Its audio is being recorded.
    Recording,
    /// Recorded but not transcribed: its transcription was cancelled.
    Recorded,
    /// Waiting for its transcription to start.
    Queued,
    /// Being transcribed; `progress` says how far.
    Transcribing,
    /// Transcribed.
    Ready,
    /// The last transcription failed; a transcript from before, if any, stays.
    Failed,
}

impl MeetingStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Recording => "recording",
            Self::Recorded => "recorded",
            Self::Queued => "queued",
            Self::Transcribing => "transcribing",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "recording" => Self::Recording,
            "recorded" => Self::Recorded,
            "queued" => Self::Queued,
            "transcribing" => Self::Transcribing,
            "ready" => Self::Ready,
            _ => Self::Failed,
        }
    }
}

/// Where a meeting's audio is. Re-transcribing and playback need `kept`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingAudio {
    Kept,
    Binned,
    Deleted,
}

impl MeetingAudio {
    fn parse(value: &str) -> Self {
        match value {
            "binned" => Self::Binned,
            "deleted" => Self::Deleted,
            _ => Self::Kept,
        }
    }
}

/// `name` is the user's name for the speaker; `null` shows the default label.
/// `talk_ms` is how long they speak in the transcript.
#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct MeetingSpeaker {
    pub key: String,
    pub name: Option<String>,
    pub talk_ms: i64,
}

/// One utterance of the transcript, times from the start of the meeting.
/// `speaker` is `me` (the microphone, one person), `them` (the other side,
/// not split into speakers) or `s1`..`s8` (diarized speakers of either side,
/// numbered in order of first appearance).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
pub struct MeetingSegment {
    pub speaker: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

/// A recorded meeting. `started_at` is in Unix seconds.
#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct Meeting {
    pub id: i64,
    /// Empty until the user names it; the UI then shows its date.
    pub title: String,
    pub started_at: i64,
    pub duration_ms: i64,
    pub status: MeetingStatus,
    /// 0-100 while `transcribing`, otherwise `null`.
    pub progress: Option<u8>,
    /// Why the last transcription failed, while `status` is `failed`.
    pub error: Option<String>,
    /// The model the current transcript was made with; `null` before one is.
    pub model_id: Option<String>,
    pub language: Option<String>,
    /// Which sides were recorded: `microphone.wav` and `system.wav`.
    pub has_microphone: bool,
    pub has_system_audio: bool,
    pub audio: MeetingAudio,
    /// The recording was cut short (the app closed or crashed) and was saved up
    /// to that point on the next launch.
    pub recovered: bool,
    /// The speakers in the transcript, in order of first appearance.
    pub speakers: Vec<MeetingSpeaker>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct MeetingDetail {
    pub meeting: Meeting,
    pub segments: Vec<MeetingSegment>,
}

/// A meeting was added or changed (`upserted`), or left the list for the Bin
/// (`removed`).
#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum MeetingUpdate {
    Upserted { meeting: Meeting },
    Removed { id: i64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingTranscriptFormat {
    Txt,
    Md,
    Srt,
    Vtt,
    Json,
}

/// A meeting in the Bin, whole (`whole`) or only its audio.
pub struct BinnedMeeting {
    pub meeting: Meeting,
    pub whole: bool,
    pub binned_at: i64,
}

/// The speakers of `segments` in order of first appearance, with how long
/// each speaks and the name the user gave them.
pub fn speakers_of(
    segments: &[MeetingSegment],
    names: &HashMap<String, String>,
) -> Vec<MeetingSpeaker> {
    let mut speakers: Vec<MeetingSpeaker> = Vec::new();
    for segment in segments {
        let talk = (segment.end_ms - segment.start_ms).max(0);
        match speakers.iter_mut().find(|s| s.key == segment.speaker) {
            Some(speaker) => speaker.talk_ms += talk,
            None => speakers.push(MeetingSpeaker {
                key: segment.speaker.clone(),
                name: names.get(&segment.speaker).cloned(),
                talk_ms: talk,
            }),
        }
    }
    speakers
}

/// A row's meeting and transcript. Progress is filled in by the store.
fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(Meeting, Vec<MeetingSegment>)> {
    let id: i64 = row.get("id")?;
    let names: String = row.get("speaker_names")?;
    let names: HashMap<String, String> = serde_json::from_str(&names).unwrap_or_else(|e| {
        warn!("Meeting {} has unreadable speaker names: {}", id, e);
        HashMap::new()
    });
    let segments: String = row.get("segments")?;
    let segments: Vec<MeetingSegment> = serde_json::from_str(&segments).unwrap_or_else(|e| {
        warn!("Meeting {} has an unreadable transcript: {}", id, e);
        Vec::new()
    });
    let status: String = row.get("status")?;
    let audio: String = row.get("audio")?;
    let meeting = Meeting {
        id,
        title: row.get("title")?,
        started_at: row.get("started_at")?,
        duration_ms: row.get("duration_ms")?,
        status: MeetingStatus::parse(&status),
        progress: None,
        error: row.get("error")?,
        model_id: row.get("model_id")?,
        language: row.get("language")?,
        has_microphone: row.get("has_microphone")?,
        has_system_audio: row.get("has_system_audio")?,
        audio: MeetingAudio::parse(&audio),
        recovered: row.get("recovered")?,
        speakers: speakers_of(&segments, &names),
    };
    Ok((meeting, segments))
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Deletes a file for good; one that's gone already counts.
fn delete_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(anyhow!("Couldn't delete {}: {}", path.display(), e)),
    }
}

/// Deletes a meeting's audio files for good: both tracks and the mix.
fn delete_audio_files(folder: &Path) -> Result<()> {
    for name in [MICROPHONE_TRACK, SYSTEM_TRACK, PLAYBACK_FILE] {
        delete_file(&folder.join(name))?;
    }
    Ok(())
}

/// Deletes a meeting's folder for good; one that's gone already counts.
fn delete_folder(folder: &Path) -> Result<()> {
    delete_audio_files(folder)?;
    match fs::remove_dir_all(folder) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(anyhow!("Couldn't delete {}: {}", folder.display(), e)),
    }
}

pub struct MeetingStore {
    app: AppHandle,
    db_path: PathBuf,
    meetings_dir: PathBuf,
    /// Percent done of each meeting being transcribed; kept in memory only.
    progress: Mutex<HashMap<i64, u8>>,
}

impl MeetingStore {
    /// Creates the table and the meetings folder.
    pub fn new(app: &AppHandle) -> Result<Self> {
        let app_data_dir = crate::portable::app_data_dir(app)?;
        let meetings_dir = app_data_dir.join("meetings");
        fs::create_dir_all(&meetings_dir)?;
        let store = Self {
            app: app.clone(),
            db_path: app_data_dir.join("history.db"),
            meetings_dir,
            progress: Mutex::new(HashMap::new()),
        };
        store.connection()?.execute_batch(MEETINGS_TABLE)?;
        Ok(store)
    }

    fn connection(&self) -> Result<Connection> {
        Ok(Connection::open(&self.db_path)?)
    }

    /// `<app_data>/meetings/<id>`.
    pub fn folder(&self, id: i64) -> PathBuf {
        self.meetings_dir.join(id.to_string())
    }

    fn with_progress(&self, mut meeting: Meeting) -> Meeting {
        if meeting.status == MeetingStatus::Transcribing {
            let progress = self.progress.lock().unwrap_or_else(|p| p.into_inner());
            meeting.progress = Some(progress.get(&meeting.id).copied().unwrap_or(0));
        }
        meeting
    }

    fn read(conn: &Connection, id: i64) -> Result<(Meeting, Vec<MeetingSegment>)> {
        conn.query_row(
            &format!("SELECT {COLUMNS} FROM meetings WHERE id = ?1"),
            params![id],
            read_row,
        )
        .optional()?
        .ok_or_else(|| anyhow!("Meeting {} not found", id))
    }

    /// A meeting, in the Bin or not.
    pub fn get(&self, id: i64) -> Result<Meeting> {
        let (meeting, _) = Self::read(&self.connection()?, id)?;
        Ok(self.with_progress(meeting))
    }

    /// A meeting with its transcript.
    pub fn detail(&self, id: i64) -> Result<MeetingDetail> {
        let (meeting, segments) = Self::read(&self.connection()?, id)?;
        Ok(MeetingDetail {
            meeting: self.with_progress(meeting),
            segments,
        })
    }

    /// Every meeting that isn't in the Bin, newest first.
    pub fn list(&self) -> Result<Vec<Meeting>> {
        let conn = self.connection()?;
        let meetings = conn
            .prepare(&format!(
                "SELECT {COLUMNS} FROM meetings WHERE binned_at IS NULL
                 ORDER BY started_at DESC, id DESC"
            ))?
            .query_map([], read_row)?
            .map(|row| row.map(|(meeting, _)| self.with_progress(meeting)))
            .collect::<rusqlite::Result<_>>()?;
        Ok(meetings)
    }

    /// Whether the meeting is in the Bin.
    pub fn is_binned(&self, id: i64) -> Result<bool> {
        let binned: Option<i64> = self
            .connection()?
            .query_row(
                "SELECT binned_at FROM meetings WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| anyhow!("Meeting {} not found", id))?;
        Ok(binned.is_some())
    }

    /// Whether the meeting has a transcript (from an earlier transcription).
    pub fn has_transcript(&self, id: i64) -> Result<bool> {
        let transcribed_at: Option<i64> = self
            .connection()?
            .query_row(
                "SELECT transcribed_at FROM meetings WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| anyhow!("Meeting {} not found", id))?;
        Ok(transcribed_at.is_some())
    }

    fn announce(&self, update: MeetingUpdate) {
        if let Err(e) = update.emit(&self.app) {
            error!("Failed to emit meeting update: {}", e);
        }
    }

    /// Reads the meeting again and announces it, unless it is in the Bin (a
    /// job ending after its meeting went there).
    fn upserted(&self, id: i64) -> Result<Meeting> {
        let meeting = self.get(id)?;
        if !self.is_binned(id)? {
            self.announce(MeetingUpdate::Upserted {
                meeting: meeting.clone(),
            });
        }
        Ok(meeting)
    }

    fn announce_bin(&self) {
        if let Some(history) = self.app.try_state::<Arc<HistoryManager>>() {
            history.announce_bin_changed();
        }
    }

    /// New row with status `recording` (has_* from what opened) and its folder created.
    pub fn create_recording(
        &self,
        started_at: i64,
        has_microphone: bool,
        has_system_audio: bool,
    ) -> Result<Meeting> {
        let conn = self.connection()?;
        conn.execute(
            "INSERT INTO meetings (started_at, status, has_microphone, has_system_audio)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                started_at,
                MeetingStatus::Recording.as_str(),
                has_microphone,
                has_system_audio
            ],
        )?;
        let id = conn.last_insert_rowid();
        if let Err(e) = fs::create_dir_all(self.folder(id)) {
            conn.execute("DELETE FROM meetings WHERE id = ?1", params![id])?;
            return Err(anyhow!("Couldn't create the meeting's folder: {}", e));
        }
        info!("Meeting {} started recording", id);
        self.upserted(id)
    }

    /// Recording saved: duration, sides that got audio, `recovered`; status -> `queued`. Emits MeetingUpdate.
    pub fn finish_recording(
        &self,
        id: i64,
        duration_ms: i64,
        has_microphone: bool,
        has_system_audio: bool,
        recovered: bool,
    ) -> Result<Meeting> {
        let updated = self.connection()?.execute(
            "UPDATE meetings SET duration_ms = ?2, has_microphone = ?3, has_system_audio = ?4,
             recovered = ?5, status = ?6, error = NULL WHERE id = ?1",
            params![
                id,
                duration_ms,
                has_microphone,
                has_system_audio,
                recovered,
                MeetingStatus::Queued.as_str()
            ],
        )?;
        if updated == 0 {
            return Err(anyhow!("Meeting {} not found", id));
        }
        info!(
            "Meeting {} recorded: {:.1}s{}",
            id,
            duration_ms as f64 / 1000.0,
            if recovered { " (recovered)" } else { "" }
        );
        self.upserted(id)
    }

    /// A start that recorded nothing: row and folder removed (not a user deletion; nothing to keep).
    pub fn discard_recording(&self, id: i64) -> Result<()> {
        self.connection()?
            .execute("DELETE FROM meetings WHERE id = ?1", params![id])?;
        if let Err(e) = delete_folder(&self.folder(id)) {
            warn!("Failed to remove the folder of meeting {}: {}", id, e);
        }
        debug!("Discarded meeting {}, which recorded nothing", id);
        self.announce(MeetingUpdate::Removed { id });
        Ok(())
    }

    /// Meetings still marked `recording` (left by a crash), for recovery.
    pub fn interrupted_recordings(&self) -> Result<Vec<Meeting>> {
        self.with_status(&[MeetingStatus::Recording])
    }

    /// Meetings whose transcription was queued or running when the app last
    /// closed, oldest first.
    pub fn unfinished_transcriptions(&self) -> Result<Vec<Meeting>> {
        let mut meetings =
            self.with_status(&[MeetingStatus::Queued, MeetingStatus::Transcribing])?;
        meetings.retain(|m| m.audio == MeetingAudio::Kept);
        meetings.reverse();
        Ok(meetings)
    }

    /// Meetings outside the Bin with one of `statuses`, newest first.
    fn with_status(&self, statuses: &[MeetingStatus]) -> Result<Vec<Meeting>> {
        Ok(self
            .list()?
            .into_iter()
            .filter(|m| statuses.contains(&m.status))
            .collect())
    }

    pub fn rename(&self, id: i64, title: &str) -> Result<Meeting> {
        let updated = self.connection()?.execute(
            "UPDATE meetings SET title = ?2 WHERE id = ?1",
            params![id, title.trim()],
        )?;
        if updated == 0 {
            return Err(anyhow!("Meeting {} not found", id));
        }
        self.upserted(id)
    }

    /// Names a speaker; `None` (or a blank name) goes back to the default label.
    pub fn rename_speaker(&self, id: i64, speaker: &str, name: Option<&str>) -> Result<Meeting> {
        let conn = self.connection()?;
        let names: String = conn
            .query_row(
                "SELECT speaker_names FROM meetings WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| anyhow!("Meeting {} not found", id))?;
        let mut names: HashMap<String, String> = serde_json::from_str(&names).unwrap_or_default();
        match name.map(str::trim).filter(|n| !n.is_empty()) {
            Some(name) => {
                names.insert(speaker.to_string(), name.to_string());
            }
            None => {
                names.remove(speaker);
            }
        }
        conn.execute(
            "UPDATE meetings SET speaker_names = ?2 WHERE id = ?1",
            params![id, serde_json::to_string(&names)?],
        )?;
        self.upserted(id)
    }

    /// Sets the status (and the error, for `failed`); progress starts over.
    pub fn set_status(
        &self,
        id: i64,
        status: MeetingStatus,
        error: Option<&str>,
    ) -> Result<Meeting> {
        self.progress
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&id);
        let updated = self.connection()?.execute(
            "UPDATE meetings SET status = ?2, error = ?3 WHERE id = ?1",
            params![id, status.as_str(), error],
        )?;
        if updated == 0 {
            return Err(anyhow!("Meeting {} not found", id));
        }
        self.upserted(id)
    }

    /// Reports how far the meeting's transcription is.
    pub fn set_progress(&self, id: i64, percent: u8) {
        self.progress
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id, percent);
        if let Err(e) = self.upserted(id) {
            error!("Failed to report meeting {}'s progress: {}", id, e);
        }
    }

    /// A finished transcription replaces the transcript; status -> `ready`.
    pub fn save_transcript(
        &self,
        id: i64,
        segments: &[MeetingSegment],
        model_id: &str,
        language: Option<&str>,
    ) -> Result<Meeting> {
        self.progress
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&id);
        let updated = self.connection()?.execute(
            "UPDATE meetings SET segments = ?2, model_id = ?3, language = ?4, transcribed_at = ?5,
             status = ?6, error = NULL WHERE id = ?1",
            params![
                id,
                serde_json::to_string(segments)?,
                model_id,
                language,
                now(),
                MeetingStatus::Ready.as_str()
            ],
        )?;
        if updated == 0 {
            return Err(anyhow!("Meeting {} not found", id));
        }
        self.upserted(id)
    }

    /// Moves the meeting, with its audio, to the Bin. The caller has made
    /// sure it isn't recording and has no transcription queued or running.
    pub fn bin(&self, id: i64) -> Result<()> {
        let updated = self.connection()?.execute(
            "UPDATE meetings SET binned_at = ?2 WHERE id = ?1 AND binned_at IS NULL",
            params![id, now()],
        )?;
        if updated == 0 {
            return Err(anyhow!("Meeting {} is not in Meetings", id));
        }
        info!("Moved meeting {} to the Bin", id);
        self.announce(MeetingUpdate::Removed { id });
        self.announce_bin();
        Ok(())
    }

    /// Moves only the meeting's audio to the Bin; the mix is deleted (it is
    /// made again from the tracks). The caller has made sure nothing uses
    /// the audio.
    pub fn bin_audio(&self, id: i64) -> Result<Meeting> {
        let meeting = self.bin_audio_quietly(id)?;
        self.announce_bin();
        Ok(meeting)
    }

    fn bin_audio_quietly(&self, id: i64) -> Result<Meeting> {
        let updated = self.connection()?.execute(
            "UPDATE meetings SET audio = 'binned', audio_binned_at = ?2
             WHERE id = ?1 AND audio = 'kept'",
            params![id, now()],
        )?;
        if updated == 0 {
            return Err(anyhow!("Meeting {} has no audio to move to the Bin", id));
        }
        if let Err(e) = delete_file(&self.folder(id).join(PLAYBACK_FILE)) {
            warn!("{}", e);
        }
        info!("Moved the audio of meeting {} to the Bin", id);
        self.upserted(id)
    }

    /// Meetings whose audio `retention` moves to the Bin at `now`, leaving
    /// those `in_use` (a transcription queued or running).
    pub fn audio_to_bin_with_conn(
        conn: &Connection,
        retention: MeetingAudioRetention,
        now: i64,
        in_use: impl Fn(i64) -> bool,
    ) -> Result<Vec<i64>> {
        let (condition, cutoff) = match retention {
            MeetingAudioRetention::Keep => return Ok(Vec::new()),
            MeetingAudioRetention::AfterTranscription => ("status = 'ready'", None),
            other => (
                "started_at < ?1 AND status NOT IN ('recording', 'queued', 'transcribing')",
                other
                    .max_age_days()
                    .map(|days| now - i64::from(days) * 24 * 60 * 60),
            ),
        };
        let query = format!(
            "SELECT id FROM meetings WHERE audio = 'kept' AND binned_at IS NULL AND {condition}
             ORDER BY id"
        );
        let mut statement = conn.prepare(&query)?;
        let ids: Vec<i64> = match cutoff {
            Some(cutoff) => statement
                .query_map(params![cutoff], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?,
            None => statement
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?,
        };
        Ok(ids.into_iter().filter(|&id| !in_use(id)).collect())
    }

    /// How many meetings' audio `retention` would move to the Bin now.
    pub fn count_audio_to_bin(
        &self,
        retention: MeetingAudioRetention,
        in_use: impl Fn(i64) -> bool,
    ) -> Result<u32> {
        Ok(
            Self::audio_to_bin_with_conn(&self.connection()?, retention, now(), in_use)?.len()
                as u32,
        )
    }

    /// Moves the audio `retention` no longer keeps to the Bin; how many
    /// meetings' audio moved.
    pub fn apply_retention(
        &self,
        retention: MeetingAudioRetention,
        in_use: impl Fn(i64) -> bool,
    ) -> Result<u32> {
        let ids = Self::audio_to_bin_with_conn(&self.connection()?, retention, now(), in_use)?;
        let mut moved = 0;
        for id in ids {
            match self.bin_audio_quietly(id) {
                Ok(_) => moved += 1,
                Err(e) => error!("Meeting audio retention: {}", e),
            }
        }
        if moved > 0 {
            info!(
                "Meeting audio retention moved {} meetings' audio to the Bin",
                moved
            );
            self.announce_bin();
        }
        Ok(moved)
    }

    /// The meetings in the Bin: whole ones, and the audio of those that
    /// aren't.
    pub fn binned_with_conn(conn: &Connection) -> Result<Vec<BinnedMeeting>> {
        let items = conn
            .prepare(&format!(
                "SELECT {COLUMNS}, binned_at, audio_binned_at FROM meetings
                 WHERE binned_at IS NOT NULL OR audio = 'binned'"
            ))?
            .query_map([], |row| {
                let (meeting, _) = read_row(row)?;
                let binned_at: Option<i64> = row.get("binned_at")?;
                let audio_binned_at: Option<i64> = row.get("audio_binned_at")?;
                Ok(match binned_at {
                    Some(binned_at) => BinnedMeeting {
                        meeting,
                        whole: true,
                        binned_at,
                    },
                    None => BinnedMeeting {
                        meeting,
                        whole: false,
                        binned_at: audio_binned_at.unwrap_or_default(),
                    },
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(items)
    }

    /// How many Bin items meetings make.
    pub fn bin_count_with_conn(conn: &Connection) -> rusqlite::Result<u32> {
        conn.query_row(
            "SELECT COUNT(*) FROM meetings WHERE binned_at IS NOT NULL OR audio = 'binned'",
            [],
            |row| row.get(0),
        )
    }

    /// A whole meeting back to Meetings; its audio stays where it was.
    pub fn restore_with_conn(conn: &Connection, id: i64) -> Result<()> {
        let restored = conn.execute(
            "UPDATE meetings SET binned_at = NULL WHERE id = ?1 AND binned_at IS NOT NULL",
            params![id],
        )?;
        if restored == 0 {
            return Err(anyhow!("Bin item {} not found", id));
        }
        Ok(())
    }

    /// A meeting's audio back to its meeting.
    pub fn restore_audio_with_conn(conn: &Connection, id: i64) -> Result<()> {
        let restored = conn.execute(
            "UPDATE meetings SET audio = 'kept', audio_binned_at = NULL
             WHERE id = ?1 AND audio = 'binned' AND binned_at IS NULL",
            params![id],
        )?;
        if restored == 0 {
            return Err(anyhow!("Bin item {} not found", id));
        }
        Ok(())
    }

    /// Deletes a whole binned meeting for good: its folder, then its row.
    /// Fails, keeping the item, when a file can't be deleted.
    pub fn delete_binned_with_conn(conn: &Connection, folder: &Path, id: i64) -> Result<()> {
        let binned: Option<Option<i64>> = conn
            .query_row(
                "SELECT binned_at FROM meetings WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        if !matches!(binned, Some(Some(_))) {
            return Err(anyhow!("Bin item {} not found", id));
        }
        delete_folder(folder)?;
        conn.execute("DELETE FROM meetings WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Deletes a meeting's binned audio for good; the meeting keeps its
    /// transcript. Fails, keeping the item, when a file can't be deleted.
    pub fn delete_binned_audio_with_conn(conn: &Connection, folder: &Path, id: i64) -> Result<()> {
        let binned: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM meetings
                 WHERE id = ?1 AND audio = 'binned' AND binned_at IS NULL",
            params![id],
            |row| row.get(0),
        )?;
        if !binned {
            return Err(anyhow!("Bin item {} not found", id));
        }
        delete_audio_files(folder)?;
        conn.execute(
            "UPDATE meetings SET audio = 'deleted', audio_binned_at = NULL WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    /// Puts a Bin item back: a whole meeting returns to Meetings, audio to
    /// its meeting. The caller announces the Bin.
    pub fn restore_bin_item(&self, id: i64, whole: bool) -> Result<()> {
        let conn = self.connection()?;
        if whole {
            Self::restore_with_conn(&conn, id)?;
        } else {
            Self::restore_audio_with_conn(&conn, id)?;
        }
        info!(
            "Restored {} {} from the Bin",
            if whole {
                "meeting"
            } else {
                "the audio of meeting"
            },
            id
        );
        self.upserted(id)?;
        Ok(())
    }

    /// Deletes a Bin item for good. The caller announces the Bin.
    pub fn delete_bin_item(&self, id: i64, whole: bool) -> Result<()> {
        let conn = self.connection()?;
        let folder = self.folder(id);
        if whole {
            Self::delete_binned_with_conn(&conn, &folder, id)?;
            info!("Deleted meeting {} from the Bin for good", id);
        } else {
            Self::delete_binned_audio_with_conn(&conn, &folder, id)?;
            info!("Deleted the audio of meeting {} from the Bin for good", id);
            self.upserted(id)?;
        }
        Ok(())
    }

    /// Deletes every meeting item in the Bin for good; how many went. An
    /// item whose files can't be deleted stays. The caller announces the Bin.
    pub fn empty_bin(&self) -> Result<u32> {
        let conn = self.connection()?;
        let mut deleted = 0;
        for item in Self::binned_with_conn(&conn)? {
            let id = item.meeting.id;
            let folder = self.folder(id);
            let result = if item.whole {
                Self::delete_binned_with_conn(&conn, &folder, id)
            } else {
                Self::delete_binned_audio_with_conn(&conn, &folder, id)
            };
            match result {
                Ok(()) => {
                    deleted += 1;
                    if !item.whole {
                        let _ = self.upserted(id);
                    }
                }
                Err(e) => warn!("Keeping meeting {} in the Bin: {}", id, e),
            }
        }
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch(MEETINGS_TABLE).expect("create meetings");
        conn
    }

    fn insert(conn: &Connection, started_at: i64, status: MeetingStatus) -> i64 {
        conn.execute(
            "INSERT INTO meetings (started_at, status, has_microphone, has_system_audio)
             VALUES (?1, ?2, 1, 1)",
            params![started_at, status.as_str()],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn segment(speaker: &str, start_ms: i64, end_ms: i64) -> MeetingSegment {
        MeetingSegment {
            speaker: speaker.into(),
            start_ms,
            end_ms,
            text: "hi".into(),
        }
    }

    #[test]
    fn speakers_come_in_order_of_first_appearance_with_their_talk_time() {
        let names = HashMap::from([("s1".to_string(), "Ana".to_string())]);
        let speakers = speakers_of(
            &[
                segment("me", 0, 1_000),
                segment("s1", 500, 2_500),
                segment("me", 3_000, 3_500),
            ],
            &names,
        );
        assert_eq!(speakers.len(), 2);
        assert_eq!(speakers[0].key, "me");
        assert_eq!(speakers[0].talk_ms, 1_500);
        assert_eq!(speakers[0].name, None);
        assert_eq!(speakers[1].key, "s1");
        assert_eq!(speakers[1].name.as_deref(), Some("Ana"));
        assert_eq!(speakers[1].talk_ms, 2_000);
    }

    #[test]
    fn retention_picks_the_audio_it_no_longer_keeps() {
        let conn = conn();
        let day = 24 * 60 * 60;
        let now = 100 * day;
        let old_ready = insert(&conn, now - 10 * day, MeetingStatus::Ready);
        let old_queued = insert(&conn, now - 10 * day, MeetingStatus::Queued);
        let old_failed = insert(&conn, now - 10 * day, MeetingStatus::Failed);
        let new_ready = insert(&conn, now - day, MeetingStatus::Ready);
        let old_binned = insert(&conn, now - 10 * day, MeetingStatus::Ready);
        conn.execute(
            "UPDATE meetings SET binned_at = 1 WHERE id = ?1",
            params![old_binned],
        )
        .unwrap();
        let none = |_| false;

        assert!(MeetingStore::audio_to_bin_with_conn(
            &conn,
            MeetingAudioRetention::Keep,
            now,
            none
        )
        .unwrap()
        .is_empty());
        assert_eq!(
            MeetingStore::audio_to_bin_with_conn(
                &conn,
                MeetingAudioRetention::AfterTranscription,
                now,
                none
            )
            .unwrap(),
            vec![old_ready, new_ready]
        );
        assert_eq!(
            MeetingStore::audio_to_bin_with_conn(&conn, MeetingAudioRetention::Days7, now, none)
                .unwrap(),
            vec![old_ready, old_failed]
        );
        assert_eq!(
            MeetingStore::audio_to_bin_with_conn(&conn, MeetingAudioRetention::Days7, now, |id| id
                == old_ready)
            .unwrap(),
            vec![old_failed]
        );
        assert!(MeetingStore::audio_to_bin_with_conn(
            &conn,
            MeetingAudioRetention::Days30,
            now,
            none
        )
        .unwrap()
        .is_empty());
        let _ = old_queued;
    }

    #[test]
    fn a_binned_meeting_comes_back_with_its_audio_where_it_was() {
        let conn = conn();
        let id = insert(&conn, 100, MeetingStatus::Ready);
        conn.execute(
            "UPDATE meetings SET audio = 'binned', audio_binned_at = 200 WHERE id = ?1",
            params![id],
        )
        .unwrap();
        let items = MeetingStore::binned_with_conn(&conn).unwrap();
        assert_eq!(items.len(), 1);
        assert!(!items[0].whole);
        assert_eq!(items[0].binned_at, 200);

        // Binned whole, it is one item holding its audio.
        conn.execute(
            "UPDATE meetings SET binned_at = 300 WHERE id = ?1",
            params![id],
        )
        .unwrap();
        let items = MeetingStore::binned_with_conn(&conn).unwrap();
        assert_eq!(items.len(), 1);
        assert!(items[0].whole);
        assert_eq!(items[0].binned_at, 300);
        assert_eq!(MeetingStore::bin_count_with_conn(&conn).unwrap(), 1);
        // Its audio can't come back on its own while the meeting is binned.
        assert!(MeetingStore::restore_audio_with_conn(&conn, id).is_err());

        MeetingStore::restore_with_conn(&conn, id).unwrap();
        let items = MeetingStore::binned_with_conn(&conn).unwrap();
        assert_eq!(items.len(), 1);
        assert!(!items[0].whole);
        assert!(MeetingStore::restore_with_conn(&conn, id).is_err());

        MeetingStore::restore_audio_with_conn(&conn, id).unwrap();
        assert!(MeetingStore::binned_with_conn(&conn).unwrap().is_empty());
        let (meeting, _) = MeetingStore::read(&conn, id).unwrap();
        assert_eq!(meeting.audio, MeetingAudio::Kept);
    }

    #[test]
    fn deleting_binned_audio_keeps_the_transcript() {
        let conn = conn();
        let dir = tempfile::TempDir::new().unwrap();
        let id = insert(&conn, 100, MeetingStatus::Ready);
        let segments = serde_json::to_string(&[segment("me", 0, 1_000)]).unwrap();
        conn.execute(
            "UPDATE meetings SET audio = 'binned', audio_binned_at = 200, segments = ?2
             WHERE id = ?1",
            params![id, segments],
        )
        .unwrap();
        for name in [MICROPHONE_TRACK, SYSTEM_TRACK, PLAYBACK_FILE, "notes.txt"] {
            fs::write(dir.path().join(name), b"x").unwrap();
        }

        MeetingStore::delete_binned_audio_with_conn(&conn, dir.path(), id).unwrap();
        for name in [MICROPHONE_TRACK, SYSTEM_TRACK, PLAYBACK_FILE] {
            assert!(!dir.path().join(name).exists());
        }
        let (meeting, segments) = MeetingStore::read(&conn, id).unwrap();
        assert_eq!(meeting.audio, MeetingAudio::Deleted);
        assert_eq!(segments.len(), 1);
        assert!(MeetingStore::binned_with_conn(&conn).unwrap().is_empty());
        // Only audio in the Bin can be deleted.
        assert!(MeetingStore::delete_binned_audio_with_conn(&conn, dir.path(), id).is_err());
    }

    #[test]
    fn deleting_a_binned_meeting_removes_its_folder_and_row() {
        let conn = conn();
        let dir = tempfile::TempDir::new().unwrap();
        let folder = dir.path().join("1");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join(MICROPHONE_TRACK), b"x").unwrap();
        let id = insert(&conn, 100, MeetingStatus::Ready);

        // Not in the Bin: nothing is deleted.
        assert!(MeetingStore::delete_binned_with_conn(&conn, &folder, id).is_err());
        assert!(folder.join(MICROPHONE_TRACK).exists());

        conn.execute(
            "UPDATE meetings SET binned_at = 300 WHERE id = ?1",
            params![id],
        )
        .unwrap();
        MeetingStore::delete_binned_with_conn(&conn, &folder, id).unwrap();
        assert!(!folder.exists());
        assert!(MeetingStore::read(&conn, id).is_err());
        assert_eq!(MeetingStore::bin_count_with_conn(&conn).unwrap(), 0);
    }
}
