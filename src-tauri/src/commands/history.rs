use crate::managers::{
    history::{BinItem, BinItemKind, HistoryEntry, HistoryManager, PaginatedHistory},
    transcription_jobs::{TranscriptionJob, TranscriptionJobManager},
};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, State};

#[tauri::command]
#[specta::specta]
pub async fn get_history_entries(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    cursor: Option<i64>,
    limit: Option<usize>,
) -> Result<PaginatedHistory, String> {
    history_manager
        .get_history_entries(cursor, limit)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn toggle_history_entry_saved(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    id: i64,
) -> Result<(), String> {
    history_manager
        .toggle_saved_status(id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn get_audio_file_path(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    file_name: String,
) -> Result<String, String> {
    if file_name.is_empty() {
        return Err("This entry has no recording".to_string());
    }
    let path = history_manager.get_audio_file_path(&file_name);
    path.to_str()
        .ok_or_else(|| "Invalid file path".to_string())
        .map(|s| s.to_string())
}

/// Moves the entry to the Bin, its recording with it. It comes back with
/// `restore_bin_item`.
#[tauri::command]
#[specta::specta]
pub async fn delete_history_entry(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    jobs: State<'_, Arc<TranscriptionJobManager>>,
    id: i64,
) -> Result<(), String> {
    jobs.cancel(id);
    history_manager.bin_entry(id).map_err(|e| e.to_string())
}

/// Queues the entry to be transcribed again with the current model. Its
/// progress and outcome arrive as `TranscriptionJob` events.
#[tauri::command]
#[specta::specta]
pub fn retry_history_entry_transcription(jobs: State<'_, Arc<TranscriptionJobManager>>, id: i64) {
    jobs.retry(id);
}

/// Stops the entry's background transcription: a queued one is dropped, a
/// running one stops after the chunk it is on. The entry keeps the text it
/// had before.
#[tauri::command]
#[specta::specta]
pub fn cancel_history_entry_transcription(jobs: State<'_, Arc<TranscriptionJobManager>>, id: i64) {
    jobs.cancel(id);
}

/// The background transcriptions queued or running now. Later changes
/// arrive as `TranscriptionJob` events.
#[tauri::command]
#[specta::specta]
pub fn get_transcription_jobs(
    jobs: State<'_, Arc<TranscriptionJobManager>>,
) -> Vec<TranscriptionJob> {
    jobs.active_jobs()
}

/// Adds an audio or video file to History and queues its transcription.
/// Fails without adding anything if the file has no audio Handy can read.
#[tauri::command]
#[specta::specta]
pub async fn transcribe_file(
    jobs: State<'_, Arc<TranscriptionJobManager>>,
    path: String,
) -> Result<HistoryEntry, String> {
    let jobs = Arc::clone(&jobs);
    tauri::async_runtime::spawn_blocking(move || jobs.transcribe_file(PathBuf::from(path)))
        .await
        .map_err(|e| format!("Failed to read the file: {}", e))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn update_history_limit(
    app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    limit: usize,
) -> Result<(), String> {
    let mut settings = crate::settings::get_settings(&app);
    settings.history_limit = limit;
    crate::settings::write_settings(&app, settings);

    history_manager
        .cleanup_old_entries()
        .map_err(|e| e.to_string())?;

    Ok(())
}

/// Everything in the Bin, most recently binned first.
#[tauri::command]
#[specta::specta]
pub async fn get_bin_items(
    history_manager: State<'_, Arc<HistoryManager>>,
) -> Result<Vec<BinItem>, String> {
    history_manager.bin_items().map_err(|e| e.to_string())
}

/// Puts a Bin item back: an entry returns to History, a recording to its
/// entry.
#[tauri::command]
#[specta::specta]
pub async fn restore_bin_item(
    history_manager: State<'_, Arc<HistoryManager>>,
    id: i64,
    kind: BinItemKind,
) -> Result<(), String> {
    history_manager
        .restore_bin_item(id, kind)
        .map_err(|e| e.to_string())
}

/// Deletes a Bin item for good, its recording file included. Fails, keeping
/// the item, when the file can't be deleted (another app has it open).
#[tauri::command]
#[specta::specta]
pub async fn delete_bin_item(
    history_manager: State<'_, Arc<HistoryManager>>,
    id: i64,
    kind: BinItemKind,
) -> Result<(), String> {
    history_manager
        .delete_bin_item(id, kind)
        .map_err(|e| e.to_string())
}

/// Deletes everything in the Bin for good; how many items went. An item
/// whose file can't be deleted stays in the Bin.
#[tauri::command]
#[specta::specta]
pub async fn empty_bin(history_manager: State<'_, Arc<HistoryManager>>) -> Result<u32, String> {
    history_manager.empty_bin().map_err(|e| e.to_string())
}

/// The recordings `bin_recordings` can move: those of History entries that
/// aren't starred and that no transcription job needs.
#[tauri::command]
#[specta::specta]
pub async fn count_recordings(
    jobs: State<'_, Arc<TranscriptionJobManager>>,
) -> Result<u32, String> {
    jobs.count_recordings().map_err(|e| e.to_string())
}

/// Moves recordings to the Bin, keeping their entries' text in History: all
/// those `count_recordings` counts, or only the ones older than
/// `older_than_days`. How many moved.
#[tauri::command]
#[specta::specta]
pub async fn bin_recordings(
    jobs: State<'_, Arc<TranscriptionJobManager>>,
    older_than_days: Option<u32>,
) -> Result<u32, String> {
    jobs.bin_recordings(older_than_days)
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn update_recording_retention_period(
    app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    period: String,
) -> Result<(), String> {
    use crate::settings::RecordingRetentionPeriod;

    let retention_period = match period.as_str() {
        "dont_save" => RecordingRetentionPeriod::DontSave,
        "never" => RecordingRetentionPeriod::Never,
        "preserve_limit" => RecordingRetentionPeriod::PreserveLimit,
        "days3" => RecordingRetentionPeriod::Days3,
        "weeks2" => RecordingRetentionPeriod::Weeks2,
        "months3" => RecordingRetentionPeriod::Months3,
        _ => return Err(format!("Invalid retention period: {}", period)),
    };

    let mut settings = crate::settings::get_settings(&app);
    settings.recording_retention_period = retention_period;
    crate::settings::write_settings(&app, settings);

    history_manager
        .cleanup_old_entries()
        .map_err(|e| e.to_string())?;

    Ok(())
}
