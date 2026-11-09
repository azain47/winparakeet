use log::error;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

use crate::managers::meeting::capture::{MeetingRecorder, MeetingRecording};
use crate::managers::meeting::export::{export_audio, playback_path, render_transcript};
use crate::managers::meeting::{
    Meeting, MeetingAudio, MeetingDetail, MeetingStore, MeetingTranscriptFormat,
};
use crate::managers::model::{ModelManager, DIARIZER_ID};
use crate::managers::transcription_jobs::TranscriptionJobManager;
use crate::settings::{get_settings, write_settings, MeetingAudioRetention, MeetingSources};

/// Runs blocking work (devices, files, the database) off the async workers.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Refuses to touch the meeting being recorded.
fn ensure_not_recording(recorder: &MeetingRecorder, id: i64) -> Result<(), String> {
    if recorder.recording().is_some_and(|r| r.meeting_id == id) {
        return Err("The meeting is still recording".to_string());
    }
    Ok(())
}

/// The meeting, which must still have its audio.
fn with_audio(store: &MeetingStore, id: i64) -> anyhow::Result<Meeting> {
    let meeting = store.get(id)?;
    if meeting.audio != MeetingAudio::Kept {
        anyhow::bail!("This meeting's audio is no longer kept");
    }
    Ok(meeting)
}

/// Starts recording a meeting from the sources `meeting_sources` asks for.
/// Fails when a meeting is already recording or no source could open.
#[tauri::command]
#[specta::specta]
pub async fn start_meeting(recorder: State<'_, Arc<MeetingRecorder>>) -> Result<Meeting, String> {
    let recorder = Arc::clone(&recorder);
    blocking(move || recorder.start()).await
}

/// Stops the meeting being recorded, saves its audio and queues its
/// transcription.
#[tauri::command]
#[specta::specta]
pub async fn stop_meeting(recorder: State<'_, Arc<MeetingRecorder>>) -> Result<Meeting, String> {
    let recorder = Arc::clone(&recorder);
    blocking(move || recorder.stop()).await
}

/// The meeting being recorded, if any.
#[tauri::command]
#[specta::specta]
pub fn get_meeting_recording(
    recorder: State<'_, Arc<MeetingRecorder>>,
) -> Result<Option<MeetingRecording>, String> {
    Ok(recorder.recording())
}

/// Every meeting that isn't in the Bin, newest first.
#[tauri::command]
#[specta::specta]
pub async fn get_meetings(store: State<'_, Arc<MeetingStore>>) -> Result<Vec<Meeting>, String> {
    let store = Arc::clone(&store);
    blocking(move || store.list()).await
}

/// A meeting with its transcript.
#[tauri::command]
#[specta::specta]
pub async fn get_meeting(
    store: State<'_, Arc<MeetingStore>>,
    id: i64,
) -> Result<MeetingDetail, String> {
    let store = Arc::clone(&store);
    blocking(move || store.detail(id)).await
}

/// Sets a meeting's title; an empty one shows its date.
#[tauri::command]
#[specta::specta]
pub async fn rename_meeting(
    store: State<'_, Arc<MeetingStore>>,
    id: i64,
    title: String,
) -> Result<(), String> {
    let store = Arc::clone(&store);
    blocking(move || store.rename(id, &title).map(|_| ())).await
}

/// Names a speaker of a meeting; `null` goes back to the default label.
/// Only the name changes, never the transcript.
#[tauri::command]
#[specta::specta]
pub async fn rename_meeting_speaker(
    store: State<'_, Arc<MeetingStore>>,
    id: i64,
    speaker: String,
    name: Option<String>,
) -> Result<(), String> {
    let store = Arc::clone(&store);
    blocking(move || {
        store
            .rename_speaker(id, &speaker, name.as_deref())
            .map(|_| ())
    })
    .await
}

/// Queues a meeting's (re)transcription with `modelId`, or with the
/// `meeting_model` setting when `null`. Its current transcript stays until the
/// new one is done. Needs the meeting's audio.
#[tauri::command]
#[specta::specta]
pub async fn transcribe_meeting(
    jobs: State<'_, Arc<TranscriptionJobManager>>,
    id: i64,
    model_id: Option<String>,
) -> Result<(), String> {
    let jobs = Arc::clone(&jobs);
    blocking(move || jobs.enqueue_meeting(id, model_id)).await
}

/// Stops a meeting's queued or running transcription; its transcript from
/// before, if any, stays.
#[tauri::command]
#[specta::specta]
pub async fn cancel_meeting_transcription(
    jobs: State<'_, Arc<TranscriptionJobManager>>,
    id: i64,
) -> Result<(), String> {
    let jobs = Arc::clone(&jobs);
    blocking(move || {
        jobs.cancel_meeting(id);
        Ok(())
    })
    .await
}

/// Moves a meeting, with its audio, to the Bin.
#[tauri::command]
#[specta::specta]
pub async fn bin_meeting(
    recorder: State<'_, Arc<MeetingRecorder>>,
    jobs: State<'_, Arc<TranscriptionJobManager>>,
    id: i64,
) -> Result<(), String> {
    ensure_not_recording(&recorder, id)?;
    let jobs = Arc::clone(&jobs);
    blocking(move || jobs.bin_meeting(id, false)).await
}

/// Moves only a meeting's audio to the Bin; its transcript stays.
#[tauri::command]
#[specta::specta]
pub async fn bin_meeting_audio(
    recorder: State<'_, Arc<MeetingRecorder>>,
    jobs: State<'_, Arc<TranscriptionJobManager>>,
    id: i64,
) -> Result<(), String> {
    ensure_not_recording(&recorder, id)?;
    let jobs = Arc::clone(&jobs);
    blocking(move || jobs.bin_meeting(id, true)).await
}

/// The meeting's audio with both sides mixed, for playback: a 16 kHz mono
/// WAV, made on first use.
#[tauri::command]
#[specta::specta]
pub async fn get_meeting_audio_path(
    recorder: State<'_, Arc<MeetingRecorder>>,
    store: State<'_, Arc<MeetingStore>>,
    id: i64,
) -> Result<String, String> {
    ensure_not_recording(&recorder, id)?;
    let store = Arc::clone(&store);
    blocking(move || {
        with_audio(&store, id)?;
        let path = playback_path(&store.folder(id))?;
        Ok(path.to_string_lossy().into_owned())
    })
    .await
}

/// Writes the mixed audio (see `get_meeting_audio_path`) to `path`.
#[tauri::command]
#[specta::specta]
pub async fn export_meeting_audio(
    recorder: State<'_, Arc<MeetingRecorder>>,
    store: State<'_, Arc<MeetingStore>>,
    id: i64,
    path: String,
) -> Result<(), String> {
    ensure_not_recording(&recorder, id)?;
    let store = Arc::clone(&store);
    blocking(move || {
        with_audio(&store, id)?;
        export_audio(&store.folder(id), &PathBuf::from(path))
    })
    .await
}

/// Writes the transcript to `path` in `format`. `speakerNames` has the name
/// to print for every speaker key in it.
#[tauri::command]
#[specta::specta]
pub async fn export_meeting_transcript(
    store: State<'_, Arc<MeetingStore>>,
    id: i64,
    format: MeetingTranscriptFormat,
    path: String,
    speaker_names: HashMap<String, String>,
) -> Result<(), String> {
    let store = Arc::clone(&store);
    blocking(move || {
        let detail = store.detail(id)?;
        let text = render_transcript(&detail.meeting, &detail.segments, format, &speaker_names);
        std::fs::write(&path, text).map_err(|e| anyhow::anyhow!("Couldn't write {}: {}", path, e))
    })
    .await
}

/// The meeting's folder, holding each side's own track (`microphone.wav`,
/// `system.wav`).
#[tauri::command]
#[specta::specta]
pub fn get_meeting_folder(store: State<'_, Arc<MeetingStore>>, id: i64) -> Result<String, String> {
    Ok(store.folder(id).to_string_lossy().into_owned())
}

/// How many meetings' audio `retention` would move to the Bin right now.
#[tauri::command]
#[specta::specta]
pub async fn count_meeting_audio_to_bin(
    jobs: State<'_, Arc<TranscriptionJobManager>>,
    retention: MeetingAudioRetention,
) -> Result<u32, String> {
    let jobs = Arc::clone(&jobs);
    blocking(move || jobs.count_meeting_audio_to_bin(retention)).await
}

#[tauri::command]
#[specta::specta]
pub fn change_meeting_sources_setting(
    app: AppHandle,
    sources: MeetingSources,
) -> Result<(), String> {
    let mut settings = get_settings(&app);
    settings.meeting_sources = sources;
    write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_meeting_model_setting(
    app: AppHandle,
    model_id: Option<String>,
) -> Result<(), String> {
    let mut settings = get_settings(&app);
    settings.meeting_model = model_id.filter(|m| !m.trim().is_empty());
    write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_meeting_speaker_labels_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = get_settings(&app);
    settings.meeting_speaker_labels = enabled;
    write_settings(&app, settings);
    Ok(())
}

/// Saves the setting and applies it: meetings whose audio it no longer keeps
/// move it to the Bin.
#[tauri::command]
#[specta::specta]
pub async fn change_meeting_audio_retention_setting(
    app: AppHandle,
    jobs: State<'_, Arc<TranscriptionJobManager>>,
    retention: MeetingAudioRetention,
) -> Result<(), String> {
    let mut settings = get_settings(&app);
    settings.meeting_audio_retention = retention;
    write_settings(&app, settings);
    let jobs = Arc::clone(&jobs);
    blocking(move || {
        jobs.apply_meeting_retention();
        Ok(())
    })
    .await
}

/// Whether the speaker-label model (Sortformer, 4 speakers) is downloaded.
#[tauri::command]
#[specta::specta]
pub fn is_diarizer_downloaded(model_manager: State<'_, Arc<ModelManager>>) -> bool {
    model_manager.diarizer_path().is_some()
}

/// Downloads the speaker-label model; progress arrives like the language
/// detector's.
#[tauri::command]
#[specta::specta]
pub async fn download_diarizer(
    app_handle: AppHandle,
    model_manager: State<'_, Arc<ModelManager>>,
) -> Result<(), String> {
    let result = model_manager
        .download_diarizer()
        .await
        .map_err(|e| e.to_string());
    if let Err(error) = &result {
        error!("Speaker diarizer download failed: {}", error);
        let _ = app_handle.emit(
            "model-download-failed",
            serde_json::json!({ "model_id": DIARIZER_ID, "error": error }),
        );
    }
    result
}
