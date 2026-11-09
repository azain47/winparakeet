use anyhow::{anyhow, Result};
use chrono::{DateTime, Local, Utc};
use log::{debug, error, info, warn};
use rusqlite::{params, Connection, OptionalExtension};
use rusqlite_migration::{MigrationDefinitionError, Migrations, M};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::fs;
use std::path::PathBuf;
use tauri::AppHandle;
use tauri_specta::Event;

use crate::language_detection::{LanguageDecision, LanguageSource};
use crate::settings::RecordingRetentionPeriod;

/// Database migrations for transcription history.
/// Each migration is applied in order. The library tracks which migrations
/// have been applied using SQLite's user_version pragma.
///
/// Note: For users upgrading from tauri-plugin-sql, migrate_from_tauri_plugin_sql()
/// converts the old _sqlx_migrations table tracking to the user_version pragma,
/// ensuring migrations don't re-run on existing databases.
static MIGRATIONS: &[M] = &[
    M::up(
        "CREATE TABLE IF NOT EXISTS transcription_history (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            file_name TEXT NOT NULL,
            timestamp INTEGER NOT NULL,
            saved BOOLEAN NOT NULL DEFAULT 0,
            title TEXT NOT NULL,
            transcription_text TEXT NOT NULL
        );",
    ),
    M::up("ALTER TABLE transcription_history ADD COLUMN post_processed_text TEXT;"),
    M::up("ALTER TABLE transcription_history ADD COLUMN post_process_prompt TEXT;"),
    M::up("ALTER TABLE transcription_history ADD COLUMN post_process_requested BOOLEAN NOT NULL DEFAULT 0;"),
];

/// Nullable columns added outside [`MIGRATIONS`]; see
/// [`HistoryManager::apply_migrations`].
const ADDED_COLUMNS: &[&str] = &["source_name", "language", "language_source"];

/// Every column a [`HistoryEntry`] is read from.
const ENTRY_COLUMNS: &str = "id, file_name, timestamp, saved, title, transcription_text, \
    post_processed_text, post_process_prompt, post_process_requested, source_name, language, \
    language_source";

/// The Bin: entries moved out of History whole, and recordings moved out of
/// entries that stay. Created outside [`MIGRATIONS`] like [`ADDED_COLUMNS`],
/// which `binned_entries` gets too. An entry whose recording went to the Bin
/// before it holds that recording, with `recording_binned_at` set so that
/// restoring the entry puts the recording back in the Bin.
const BIN_TABLES: &str = "
    CREATE TABLE IF NOT EXISTS binned_entries (
        id INTEGER PRIMARY KEY,
        file_name TEXT NOT NULL,
        timestamp INTEGER NOT NULL,
        saved BOOLEAN NOT NULL DEFAULT 0,
        title TEXT NOT NULL,
        transcription_text TEXT NOT NULL,
        post_processed_text TEXT,
        post_process_prompt TEXT,
        post_process_requested BOOLEAN NOT NULL DEFAULT 0,
        binned_at INTEGER NOT NULL,
        recording_binned_at INTEGER
    );
    CREATE TABLE IF NOT EXISTS binned_recordings (
        entry_id INTEGER PRIMARY KEY,
        file_name TEXT NOT NULL,
        binned_at INTEGER NOT NULL
    );";

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct PaginatedHistory {
    pub entries: Vec<HistoryEntry>,
    pub has_more: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
#[serde(tag = "action")]
pub enum HistoryUpdatePayload {
    #[serde(rename = "added")]
    Added { entry: HistoryEntry },
    #[serde(rename = "updated")]
    Updated { entry: HistoryEntry },
    #[serde(rename = "deleted")]
    Deleted { id: i64 },
    #[serde(rename = "toggled")]
    Toggled { id: i64 },
}

/// What a Bin item holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum BinItemKind {
    /// A whole History entry, with its recording if it had one.
    Entry,
    /// Only a recording; its entry is still in History, keeping its text.
    Recording,
}

/// Something in the Bin. Nothing leaves the Bin except by restoring it or
/// deleting it for good.
#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct BinItem {
    /// The entry it belongs to. A `recording` item's entry is still in History.
    pub entry: HistoryEntry,
    pub kind: BinItemKind,
    /// When it went to the Bin, in Unix seconds.
    pub binned_at: i64,
    /// Its recording in the recordings folder; empty when it has none.
    pub recording: String,
}

/// The Bin's contents changed; `count` items are in it now.
#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct BinChanged {
    pub count: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct HistoryEntry {
    pub id: i64,
    /// The recording in the recordings folder; empty when none is kept.
    pub file_name: String,
    pub timestamp: i64,
    pub saved: bool,
    pub title: String,
    pub transcription_text: String,
    pub post_processed_text: Option<String>,
    pub post_process_prompt: Option<String>,
    pub post_process_requested: bool,
    /// The name of the file this entry was transcribed from; `None` for a
    /// dictation.
    pub source_name: Option<String>,
    /// The language it was transcribed in, when known.
    pub language: Option<String>,
    /// How that language was chosen; `None` before it was recorded.
    pub language_source: Option<LanguageSource>,
}

pub struct HistoryManager {
    app_handle: AppHandle,
    recordings_dir: PathBuf,
    db_path: PathBuf,
}

/// A transcription's language as its two columns.
fn split_language(language: Option<LanguageDecision>) -> (Option<String>, Option<LanguageSource>) {
    match language {
        Some(decision) => (decision.language, Some(decision.source)),
        None => (None, None),
    }
}

impl HistoryManager {
    pub fn new(app_handle: &AppHandle) -> Result<Self> {
        // Create recordings directory in app data dir
        let app_data_dir = crate::portable::app_data_dir(app_handle)?;
        let recordings_dir = app_data_dir.join("recordings");
        let db_path = app_data_dir.join("history.db");

        // Ensure recordings directory exists
        if !recordings_dir.exists() {
            fs::create_dir_all(&recordings_dir)?;
            debug!("Created recordings directory: {:?}", recordings_dir);
        }

        let manager = Self {
            app_handle: app_handle.clone(),
            recordings_dir,
            db_path,
        };

        // Initialize database and run migrations synchronously
        manager.init_database()?;

        Ok(manager)
    }

    fn init_database(&self) -> Result<()> {
        info!("Initializing database at {:?}", self.db_path);

        let mut conn = Connection::open(&self.db_path)?;

        // Handle migration from tauri-plugin-sql to rusqlite_migration
        // tauri-plugin-sql used _sqlx_migrations table, rusqlite_migration uses user_version pragma
        self.migrate_from_tauri_plugin_sql(&conn)?;

        // Validate migrations in debug builds
        #[cfg(debug_assertions)]
        Migrations::new(MIGRATIONS.to_vec())
            .validate()
            .expect("Invalid migrations");

        // Get current version before migration
        let version_before: i32 =
            conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        debug!("Database version before migration: {}", version_before);

        Self::apply_migrations(&mut conn)?;

        // Get version after migration
        let version_after: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;

        if version_after > version_before {
            info!(
                "Database migrated from version {} to {}",
                version_before, version_after
            );
        } else {
            debug!("Database already at latest version {}", version_after);
        }

        Ok(())
    }

    /// Bring the schema up to date. A newer Handy's database opens as is:
    /// migrations only add columns, and every query names its columns.
    fn apply_migrations(conn: &mut Connection) -> Result<()> {
        match Migrations::new(MIGRATIONS.to_vec()).to_latest(conn) {
            Err(rusqlite_migration::Error::MigrationDefinition(
                MigrationDefinitionError::DatabaseTooFarAhead,
            )) => {
                warn!("history.db is from a newer version of Handy; opening it without migrating")
            }
            other => other?,
        }

        // Not migrations: a higher user_version makes released builds refuse
        // the database, while an extra table or nullable column is invisible
        // to them.
        conn.execute_batch(BIN_TABLES)?;
        for table in ["transcription_history", "binned_entries"] {
            for column in ADDED_COLUMNS {
                if !Self::has_column(conn, table, column)? {
                    conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} TEXT"), [])?;
                    info!("Added the {column} column to {table}");
                }
            }
        }

        Ok(())
    }

    fn has_column(conn: &Connection, table: &str, name: &str) -> Result<bool> {
        Ok(conn.query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info(?1) WHERE name = ?2",
            [table, name],
            |row| row.get(0),
        )?)
    }

    /// Migrate from tauri-plugin-sql's migration tracking to rusqlite_migration's.
    /// tauri-plugin-sql used a _sqlx_migrations table, while rusqlite_migration uses
    /// SQLite's user_version pragma. This function checks if the old system was in use
    /// and sets the user_version accordingly so migrations don't re-run.
    fn migrate_from_tauri_plugin_sql(&self, conn: &Connection) -> Result<()> {
        // Check if the old _sqlx_migrations table exists
        let has_sqlx_migrations: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='_sqlx_migrations'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);

        if !has_sqlx_migrations {
            return Ok(());
        }

        // Check current user_version
        let current_version: i32 =
            conn.pragma_query_value(None, "user_version", |row| row.get(0))?;

        if current_version > 0 {
            // Already migrated to rusqlite_migration system
            return Ok(());
        }

        // Get the highest version from the old migrations table
        let old_version: i32 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM _sqlx_migrations WHERE success = 1",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        if old_version > 0 {
            info!(
                "Migrating from tauri-plugin-sql (version {}) to rusqlite_migration",
                old_version
            );

            // Set user_version to match the old migration state
            conn.pragma_update(None, "user_version", old_version)?;

            // Optionally drop the old migrations table (keeping it doesn't hurt)
            // conn.execute("DROP TABLE IF EXISTS _sqlx_migrations", [])?;

            info!(
                "Migration tracking converted: user_version set to {}",
                old_version
            );
        }

        Ok(())
    }

    fn get_connection(&self) -> Result<Connection> {
        Ok(Connection::open(&self.db_path)?)
    }

    fn map_history_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryEntry> {
        Ok(HistoryEntry {
            id: row.get("id")?,
            file_name: row.get("file_name")?,
            timestamp: row.get("timestamp")?,
            saved: row.get("saved")?,
            title: row.get("title")?,
            transcription_text: row.get("transcription_text")?,
            post_processed_text: row.get("post_processed_text")?,
            post_process_prompt: row.get("post_process_prompt")?,
            post_process_requested: row.get("post_process_requested")?,
            source_name: row.get("source_name")?,
            language: row.get("language")?,
            language_source: row
                .get::<_, Option<String>>("language_source")?
                .as_deref()
                .and_then(LanguageSource::parse),
        })
    }

    pub fn recordings_dir(&self) -> &std::path::Path {
        &self.recordings_dir
    }

    /// Save a new history entry to the database.
    /// The WAV file should already have been written to the recordings
    /// directory; `file_name` is empty when no recording is kept.
    pub fn save_entry(
        &self,
        file_name: String,
        transcription_text: String,
        post_process_requested: bool,
        post_processed_text: Option<String>,
        post_process_prompt: Option<String>,
        language: Option<LanguageDecision>,
    ) -> Result<HistoryEntry> {
        let (language, language_source) = split_language(language);
        let entry = self.insert_entry(HistoryEntry {
            id: 0,
            file_name,
            timestamp: Utc::now().timestamp(),
            saved: false,
            title: String::new(),
            transcription_text,
            post_processed_text,
            post_process_prompt,
            post_process_requested,
            source_name: None,
            language,
            language_source,
        })?;

        self.cleanup_old_entries()?;

        Ok(entry)
    }

    /// Save the entry for a file whose audio is already at `file_name`.
    /// Retention never removes it (only deleting it does); never
    /// post-processed.
    pub fn save_file_entry(&self, file_name: String, source_name: String) -> Result<HistoryEntry> {
        self.insert_entry(HistoryEntry {
            id: 0,
            file_name,
            timestamp: Utc::now().timestamp(),
            saved: false,
            title: String::new(),
            transcription_text: String::new(),
            post_processed_text: None,
            post_process_prompt: None,
            post_process_requested: false,
            source_name: Some(source_name),
            language: None,
            language_source: None,
        })
    }

    /// Insert `entry`, filling in its id and title, and announce it.
    fn insert_entry(&self, mut entry: HistoryEntry) -> Result<HistoryEntry> {
        entry.title = self.format_timestamp_title(entry.timestamp);

        let conn = self.get_connection()?;
        conn.execute(
            "INSERT INTO transcription_history (
                file_name,
                timestamp,
                saved,
                title,
                transcription_text,
                post_processed_text,
                post_process_prompt,
                post_process_requested,
                source_name,
                language,
                language_source
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                &entry.file_name,
                entry.timestamp,
                entry.saved,
                &entry.title,
                &entry.transcription_text,
                &entry.post_processed_text,
                &entry.post_process_prompt,
                entry.post_process_requested,
                &entry.source_name,
                &entry.language,
                entry.language_source.map(LanguageSource::as_str),
            ],
        )?;
        entry.id = conn.last_insert_rowid();

        debug!("Saved history entry with id {}", entry.id);

        // Emit typed event for real-time frontend updates
        if let Err(e) = (HistoryUpdatePayload::Added {
            entry: entry.clone(),
        })
        .emit(&self.app_handle)
        {
            error!("Failed to emit history-updated event: {}", e);
        }

        Ok(entry)
    }

    /// Update an existing history entry with new transcription results (used by retry).
    pub fn update_transcription(
        &self,
        id: i64,
        transcription_text: String,
        post_processed_text: Option<String>,
        post_process_prompt: Option<String>,
        language: Option<LanguageDecision>,
    ) -> Result<HistoryEntry> {
        let (language, language_source) = split_language(language);
        let conn = self.get_connection()?;
        let updated = conn.execute(
            "UPDATE transcription_history
             SET transcription_text = ?1,
                 post_processed_text = ?2,
                 post_process_prompt = ?3,
                 language = ?4,
                 language_source = ?5
             WHERE id = ?6",
            params![
                transcription_text,
                post_processed_text,
                post_process_prompt,
                language,
                language_source.map(LanguageSource::as_str),
                id
            ],
        )?;

        if updated == 0 {
            return Err(anyhow!("History entry {} not found", id));
        }

        let entry = conn.query_row(
            &format!("SELECT {ENTRY_COLUMNS} FROM transcription_history WHERE id = ?1"),
            params![id],
            Self::map_history_entry,
        )?;

        debug!("Updated transcription for history entry {}", id);

        if let Err(e) = (HistoryUpdatePayload::Updated {
            entry: entry.clone(),
        })
        .emit(&self.app_handle)
        {
            error!("Failed to emit history-updated event: {}", e);
        }

        Ok(entry)
    }

    /// Moves the entries that the retention setting no longer keeps to the Bin.
    pub fn cleanup_old_entries(&self) -> Result<()> {
        let retention_period = crate::settings::get_recording_retention_period(&self.app_handle);

        match retention_period {
            // Not saving new recordings leaves the ones saved before alone.
            RecordingRetentionPeriod::DontSave | RecordingRetentionPeriod::Never => Ok(()),
            RecordingRetentionPeriod::PreserveLimit => {
                // Use the old count-based logic with history_limit
                let limit = crate::settings::get_history_limit(&self.app_handle);
                self.cleanup_by_count(limit)
            }
            _ => {
                // Use time-based logic
                self.cleanup_by_time(retention_period)
            }
        }
    }

    fn cleanup_by_count(&self, limit: usize) -> Result<()> {
        let conn = self.get_connection()?;

        // Unsaved dictations, newest first. Transcribed files only go to the
        // Bin explicitly, and don't count toward the limit.
        let ids: Vec<i64> = conn
            .prepare(
                "SELECT id FROM transcription_history WHERE saved = 0 AND source_name IS NULL ORDER BY timestamp DESC",
            )?
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;

        match ids.get(limit..) {
            Some(old) => self.bin_old_entries(old),
            None => Ok(()),
        }
    }

    fn cleanup_by_time(&self, retention_period: RecordingRetentionPeriod) -> Result<()> {
        let conn = self.get_connection()?;

        // Calculate cutoff timestamp (current time minus retention period)
        let now = Utc::now().timestamp();
        let cutoff_timestamp = match retention_period {
            RecordingRetentionPeriod::Days3 => now - (3 * 24 * 60 * 60), // 3 days in seconds
            RecordingRetentionPeriod::Weeks2 => now - (2 * 7 * 24 * 60 * 60), // 2 weeks in seconds
            RecordingRetentionPeriod::Months3 => now - (3 * 30 * 24 * 60 * 60), // 3 months in seconds (approximate)
            _ => unreachable!("Should not reach here"),
        };

        // Unsaved dictations older than the cutoff; transcribed files only go
        // to the Bin explicitly.
        let ids: Vec<i64> = conn
            .prepare(
                "SELECT id FROM transcription_history WHERE saved = 0 AND source_name IS NULL AND timestamp < ?1",
            )?
            .query_map(params![cutoff_timestamp], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;

        self.bin_old_entries(&ids)
    }

    /// Moves entries that the retention setting no longer keeps to the Bin.
    fn bin_old_entries(&self, ids: &[i64]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let mut conn = self.get_connection()?;
        let binned = Self::bin_entries_with_conn(&mut conn, ids, Utc::now().timestamp())?;
        debug!("Moved {} old history entries to the Bin", binned.len());
        for id in binned {
            self.announce(HistoryUpdatePayload::Deleted { id });
        }
        self.announce_bin(&conn);
        Ok(())
    }

    /// Moves the entry to the Bin, its recording with it.
    pub fn bin_entry(&self, id: i64) -> Result<()> {
        let mut conn = self.get_connection()?;
        if Self::bin_entries_with_conn(&mut conn, &[id], Utc::now().timestamp())?.is_empty() {
            return Err(anyhow!("History entry {} not found", id));
        }
        debug!("Moved history entry {} to the Bin", id);
        self.announce(HistoryUpdatePayload::Deleted { id });
        self.announce_bin(&conn);
        Ok(())
    }

    /// Moves the entries to the Bin; the ids of those that were in History.
    /// An entry whose recording is in the Bin already holds it there, so the
    /// Bin shows them as one item.
    fn bin_entries_with_conn(conn: &mut Connection, ids: &[i64], now: i64) -> Result<Vec<i64>> {
        let tx = conn.transaction()?;
        let mut binned = Vec::new();
        for &id in ids {
            let moved = tx.execute(
                &format!(
                    "INSERT INTO binned_entries ({ENTRY_COLUMNS}, binned_at)
                     SELECT {ENTRY_COLUMNS}, ?2 FROM transcription_history WHERE id = ?1"
                ),
                params![id, now],
            )?;
            if moved == 0 {
                continue;
            }
            tx.execute(
                "UPDATE binned_entries
                 SET file_name = (SELECT file_name FROM binned_recordings WHERE entry_id = ?1),
                     recording_binned_at = (SELECT binned_at FROM binned_recordings WHERE entry_id = ?1)
                 WHERE id = ?1 AND EXISTS (SELECT 1 FROM binned_recordings WHERE entry_id = ?1)",
                params![id],
            )?;
            tx.execute(
                "DELETE FROM binned_recordings WHERE entry_id = ?1",
                params![id],
            )?;
            tx.execute(
                "DELETE FROM transcription_history WHERE id = ?1",
                params![id],
            )?;
            binned.push(id);
        }
        tx.commit()?;
        Ok(binned)
    }

    /// Moves the entry's recording to the Bin, if it has one; the entry keeps
    /// its text.
    pub fn bin_recording(&self, id: i64) -> Result<()> {
        self.bin_recordings_of(&[id]).map(|_| ())
    }

    /// How many recordings `bin_recordings` would move without a cutoff.
    pub fn count_recordings(&self, in_use: impl Fn(i64) -> bool) -> Result<u32> {
        let conn = self.get_connection()?;
        Ok(Self::recordings_to_bin(&conn, None, in_use)?.len() as u32)
    }

    /// Moves recordings to the Bin, their entries keeping their text: those
    /// of entries that aren't starred and that `in_use` doesn't claim, and
    /// with `older_than_days`, only older ones. How many moved.
    pub fn bin_recordings(
        &self,
        older_than_days: Option<u32>,
        in_use: impl Fn(i64) -> bool,
    ) -> Result<u32> {
        let cutoff =
            older_than_days.map(|days| Utc::now().timestamp() - i64::from(days) * 24 * 60 * 60);
        let ids = Self::recordings_to_bin(&self.get_connection()?, cutoff, in_use)?;
        let moved = self.bin_recordings_of(&ids)?;
        info!("Moved {} recordings to the Bin", moved);
        Ok(moved)
    }

    /// Entries with a recording that aren't starred, that `in_use` doesn't
    /// claim, and that are older than `cutoff` (Unix seconds) if there is one.
    fn recordings_to_bin(
        conn: &Connection,
        cutoff: Option<i64>,
        in_use: impl Fn(i64) -> bool,
    ) -> Result<Vec<i64>> {
        let ids: Vec<i64> = conn
            .prepare(
                "SELECT id FROM transcription_history WHERE file_name != '' AND saved = 0 AND timestamp < ?1 ORDER BY id",
            )?
            .query_map(params![cutoff.unwrap_or(i64::MAX)], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(ids.into_iter().filter(|&id| !in_use(id)).collect())
    }

    fn bin_recordings_of(&self, ids: &[i64]) -> Result<u32> {
        let mut conn = self.get_connection()?;
        let entries = Self::bin_recordings_with_conn(&mut conn, ids, Utc::now().timestamp())?;
        let moved = entries.len() as u32;
        for entry in entries {
            self.announce(HistoryUpdatePayload::Updated { entry });
        }
        if moved > 0 {
            self.announce_bin(&conn);
        }
        Ok(moved)
    }

    /// Moves the entries' recordings to the Bin; the entries, now without one.
    fn bin_recordings_with_conn(
        conn: &mut Connection,
        ids: &[i64],
        now: i64,
    ) -> Result<Vec<HistoryEntry>> {
        let tx = conn.transaction()?;
        let mut entries = Vec::new();
        for &id in ids {
            let moved = tx.execute(
                "INSERT INTO binned_recordings (entry_id, file_name, binned_at)
                 SELECT id, file_name, ?2 FROM transcription_history WHERE id = ?1 AND file_name != ''",
                params![id, now],
            )?;
            if moved == 0 {
                continue;
            }
            tx.execute(
                "UPDATE transcription_history SET file_name = '' WHERE id = ?1",
                params![id],
            )?;
            entries.push(Self::entry_with_conn(&tx, id)?);
        }
        tx.commit()?;
        Ok(entries)
    }

    /// Everything in the Bin, most recently binned first.
    pub fn bin_items(&self) -> Result<Vec<BinItem>> {
        Self::bin_items_with_conn(&self.get_connection()?)
    }

    fn bin_items_with_conn(conn: &Connection) -> Result<Vec<BinItem>> {
        let mut items: Vec<BinItem> = conn
            .prepare(&format!(
                "SELECT {ENTRY_COLUMNS}, binned_at FROM binned_entries"
            ))?
            .query_map([], |row| {
                let entry = Self::map_history_entry(row)?;
                Ok(BinItem {
                    recording: entry.file_name.clone(),
                    entry,
                    kind: BinItemKind::Entry,
                    binned_at: row.get("binned_at")?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;

        // A binned recording's entry is in History: binning the entry takes
        // the recording back.
        let recordings: Vec<(i64, String, i64)> = conn
            .prepare("SELECT entry_id, file_name, binned_at FROM binned_recordings")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (id, recording, binned_at) in recordings {
            if let Some(entry) = Self::entry_with_conn(conn, id).optional()? {
                items.push(BinItem {
                    entry,
                    kind: BinItemKind::Recording,
                    binned_at,
                    recording,
                });
            }
        }

        items.sort_by(|a, b| {
            b.binned_at
                .cmp(&a.binned_at)
                .then(b.entry.id.cmp(&a.entry.id))
        });
        Ok(items)
    }

    /// Puts a Bin item back: an entry returns to History, a recording to its
    /// entry.
    pub fn restore_bin_item(&self, id: i64, kind: BinItemKind) -> Result<()> {
        let mut conn = self.get_connection()?;
        let payload = match kind {
            BinItemKind::Entry => HistoryUpdatePayload::Added {
                entry: Self::restore_entry_with_conn(&mut conn, id)?,
            },
            BinItemKind::Recording => HistoryUpdatePayload::Updated {
                entry: Self::restore_recording_with_conn(&mut conn, id)?,
            },
        };
        debug!("Restored {:?} {} from the Bin", kind, id);
        self.announce(payload);
        self.announce_bin(&conn);
        Ok(())
    }

    /// The entry goes back to History with its id, so it sorts where it was,
    /// and a recording that was in the Bin before it goes back to the Bin.
    fn restore_entry_with_conn(conn: &mut Connection, id: i64) -> Result<HistoryEntry> {
        let tx = conn.transaction()?;
        let binned_recording: Option<(String, i64)> = tx
            .query_row(
                "SELECT file_name, recording_binned_at FROM binned_entries
                 WHERE id = ?1 AND recording_binned_at IS NOT NULL",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let restored = tx.execute(
            &format!(
                "INSERT INTO transcription_history ({ENTRY_COLUMNS})
                 SELECT {ENTRY_COLUMNS} FROM binned_entries WHERE id = ?1"
            ),
            params![id],
        )?;
        if restored == 0 {
            return Err(anyhow!("Bin item {} not found", id));
        }
        if let Some((file_name, binned_at)) = binned_recording {
            tx.execute(
                "INSERT INTO binned_recordings (entry_id, file_name, binned_at) VALUES (?1, ?2, ?3)",
                params![id, file_name, binned_at],
            )?;
            tx.execute(
                "UPDATE transcription_history SET file_name = '' WHERE id = ?1",
                params![id],
            )?;
        }
        tx.execute("DELETE FROM binned_entries WHERE id = ?1", params![id])?;
        let entry = Self::entry_with_conn(&tx, id)?;
        tx.commit()?;
        Ok(entry)
    }

    fn restore_recording_with_conn(conn: &mut Connection, id: i64) -> Result<HistoryEntry> {
        let tx = conn.transaction()?;
        let file_name: String = tx
            .query_row(
                "SELECT file_name FROM binned_recordings WHERE entry_id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| anyhow!("Bin item {} not found", id))?;
        let restored = tx.execute(
            "UPDATE transcription_history SET file_name = ?2 WHERE id = ?1 AND file_name = ''",
            params![id, file_name],
        )?;
        if restored == 0 {
            return Err(anyhow!(
                "History entry {} can't take its recording back",
                id
            ));
        }
        tx.execute(
            "DELETE FROM binned_recordings WHERE entry_id = ?1",
            params![id],
        )?;
        let entry = Self::entry_with_conn(&tx, id)?;
        tx.commit()?;
        Ok(entry)
    }

    /// Deletes a Bin item for good, its recording file included. Fails,
    /// keeping the item, when the file can't be deleted.
    pub fn delete_bin_item(&self, id: i64, kind: BinItemKind) -> Result<()> {
        let conn = self.get_connection()?;
        let (select, delete) = Self::bin_item_queries(kind);
        let file_name: String = conn
            .query_row(select, params![id], |row| row.get(0))
            .optional()?
            .ok_or_else(|| anyhow!("Bin item {} not found", id))?;
        self.delete_recording_file(&file_name)?;
        conn.execute(delete, params![id])?;
        info!("Deleted {:?} {} from the Bin for good", kind, id);
        self.announce_bin(&conn);
        Ok(())
    }

    /// Deletes everything in the Bin for good; how many items went. An item
    /// whose file can't be deleted stays.
    pub fn empty_bin(&self) -> Result<u32> {
        let conn = self.get_connection()?;
        let mut items: Vec<(BinItemKind, i64, String)> = Vec::new();
        for kind in [BinItemKind::Entry, BinItemKind::Recording] {
            let query = match kind {
                BinItemKind::Entry => "SELECT id, file_name FROM binned_entries",
                BinItemKind::Recording => "SELECT entry_id, file_name FROM binned_recordings",
            };
            let rows: Vec<(i64, String)> = conn
                .prepare(query)?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            items.extend(rows.into_iter().map(|(id, file)| (kind, id, file)));
        }

        let mut deleted = 0;
        for (kind, id, file_name) in items {
            if let Err(e) = self.delete_recording_file(&file_name) {
                warn!("Keeping {:?} {} in the Bin: {}", kind, id, e);
                continue;
            }
            conn.execute(Self::bin_item_queries(kind).1, params![id])?;
            deleted += 1;
        }
        info!("Emptied the Bin: {} items deleted for good", deleted);
        self.announce_bin(&conn);
        Ok(deleted)
    }

    /// The queries that read a Bin item's file name and delete the item.
    fn bin_item_queries(kind: BinItemKind) -> (&'static str, &'static str) {
        match kind {
            BinItemKind::Entry => (
                "SELECT file_name FROM binned_entries WHERE id = ?1",
                "DELETE FROM binned_entries WHERE id = ?1",
            ),
            BinItemKind::Recording => (
                "SELECT file_name FROM binned_recordings WHERE entry_id = ?1",
                "DELETE FROM binned_recordings WHERE entry_id = ?1",
            ),
        }
    }

    /// Deletes a recording file for good; one that's gone already counts.
    fn delete_recording_file(&self, file_name: &str) -> Result<()> {
        if file_name.is_empty() {
            return Ok(());
        }
        match fs::remove_file(self.recordings_dir.join(file_name)) {
            Ok(()) => {
                debug!("Deleted WAV file: {}", file_name);
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(anyhow!("Couldn't delete {}: {}", file_name, e)),
        }
    }

    fn entry_with_conn(conn: &Connection, id: i64) -> rusqlite::Result<HistoryEntry> {
        conn.query_row(
            &format!("SELECT {ENTRY_COLUMNS} FROM transcription_history WHERE id = ?1"),
            params![id],
            Self::map_history_entry,
        )
    }

    fn announce(&self, payload: HistoryUpdatePayload) {
        if let Err(e) = payload.emit(&self.app_handle) {
            error!("Failed to emit history-updated event: {}", e);
        }
    }

    /// Tells the UI how many items the Bin holds now.
    fn announce_bin(&self, conn: &Connection) {
        let count = conn.query_row(
            "SELECT (SELECT COUNT(*) FROM binned_entries) + (SELECT COUNT(*) FROM binned_recordings)",
            [],
            |row| row.get(0),
        );
        match count {
            Ok(count) => {
                if let Err(e) = (BinChanged { count }).emit(&self.app_handle) {
                    error!("Failed to emit bin-changed event: {}", e);
                }
            }
            Err(e) => error!("Failed to count the Bin: {}", e),
        }
    }

    pub async fn get_history_entries(
        &self,
        cursor: Option<i64>,
        limit: Option<usize>,
    ) -> Result<PaginatedHistory> {
        let conn = self.get_connection()?;
        let limit = limit.map(|l| l.min(100));

        let mut entries: Vec<HistoryEntry> = match (cursor, limit) {
            (Some(cursor_id), Some(lim)) => {
                let fetch_count = (lim + 1) as i64;
                let mut stmt = conn.prepare(&format!(
                    "SELECT {ENTRY_COLUMNS}
                     FROM transcription_history
                     WHERE id < ?1
                     ORDER BY id DESC
                     LIMIT ?2"
                ))?;
                let result = stmt
                    .query_map(params![cursor_id, fetch_count], Self::map_history_entry)?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                result
            }
            (None, Some(lim)) => {
                let fetch_count = (lim + 1) as i64;
                let mut stmt = conn.prepare(&format!(
                    "SELECT {ENTRY_COLUMNS}
                     FROM transcription_history
                     ORDER BY id DESC
                     LIMIT ?1"
                ))?;
                let result = stmt
                    .query_map(params![fetch_count], Self::map_history_entry)?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                result
            }
            (_, None) => {
                let mut stmt = conn.prepare(&format!(
                    "SELECT {ENTRY_COLUMNS}
                     FROM transcription_history
                     ORDER BY id DESC"
                ))?;
                let result = stmt
                    .query_map([], Self::map_history_entry)?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                result
            }
        };

        let has_more = limit.is_some_and(|lim| entries.len() > lim);
        if has_more {
            entries.pop();
        }

        Ok(PaginatedHistory { entries, has_more })
    }

    #[cfg(test)]
    fn get_latest_entry_with_conn(conn: &Connection) -> Result<Option<HistoryEntry>> {
        let mut stmt = conn.prepare(&format!(
            "SELECT {ENTRY_COLUMNS}
             FROM transcription_history
             ORDER BY timestamp DESC
             LIMIT 1"
        ))?;

        let entry = stmt.query_row([], Self::map_history_entry).optional()?;
        Ok(entry)
    }

    /// Get the latest dictation with non-empty transcription text. Files are
    /// left out: a long file's transcript is never the "last transcript".
    pub fn get_latest_completed_entry(&self) -> Result<Option<HistoryEntry>> {
        let conn = self.get_connection()?;
        Self::get_latest_completed_entry_with_conn(&conn)
    }

    fn get_latest_completed_entry_with_conn(conn: &Connection) -> Result<Option<HistoryEntry>> {
        let mut stmt = conn.prepare(&format!(
            "SELECT {ENTRY_COLUMNS}
             FROM transcription_history
             WHERE transcription_text != '' AND source_name IS NULL
             ORDER BY timestamp DESC
             LIMIT 1"
        ))?;

        let entry = stmt.query_row([], Self::map_history_entry).optional()?;
        Ok(entry)
    }

    pub async fn toggle_saved_status(&self, id: i64) -> Result<()> {
        let conn = self.get_connection()?;

        // Get current saved status
        let current_saved: bool = conn.query_row(
            "SELECT saved FROM transcription_history WHERE id = ?1",
            params![id],
            |row| row.get("saved"),
        )?;

        let new_saved = !current_saved;

        conn.execute(
            "UPDATE transcription_history SET saved = ?1 WHERE id = ?2",
            params![new_saved, id],
        )?;

        debug!("Toggled saved status for entry {}: {}", id, new_saved);

        // Emit history updated event
        if let Err(e) = (HistoryUpdatePayload::Toggled { id }).emit(&self.app_handle) {
            error!("Failed to emit history-updated event: {}", e);
        }

        Ok(())
    }

    pub fn get_audio_file_path(&self, file_name: &str) -> PathBuf {
        self.recordings_dir.join(file_name)
    }

    pub async fn get_entry_by_id(&self, id: i64) -> Result<Option<HistoryEntry>> {
        let conn = self.get_connection()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {ENTRY_COLUMNS}
             FROM transcription_history
             WHERE id = ?1"
        ))?;

        let entry = stmt.query_row([id], Self::map_history_entry).optional()?;

        Ok(entry)
    }

    fn format_timestamp_title(&self, timestamp: i64) -> String {
        if let Some(utc_datetime) = DateTime::from_timestamp(timestamp, 0) {
            // Convert UTC to local timezone
            let local_datetime = utc_datetime.with_timezone(&Local);
            local_datetime.format("%B %e, %Y - %l:%M%p").to_string()
        } else {
            format!("Recording {}", timestamp)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{params, Connection};

    fn setup_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch(
            "CREATE TABLE transcription_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                file_name TEXT NOT NULL,
                timestamp INTEGER NOT NULL,
                saved BOOLEAN NOT NULL DEFAULT 0,
                title TEXT NOT NULL,
                transcription_text TEXT NOT NULL,
                post_processed_text TEXT,
                post_process_prompt TEXT,
                post_process_requested BOOLEAN NOT NULL DEFAULT 0,
                source_name TEXT,
                language TEXT,
                language_source TEXT
            );",
        )
        .expect("create transcription_history table");
        conn
    }

    fn insert_entry(conn: &Connection, timestamp: i64, text: &str, post_processed: Option<&str>) {
        conn.execute(
            "INSERT INTO transcription_history (
                file_name,
                timestamp,
                saved,
                title,
                transcription_text,
                post_processed_text,
                post_process_prompt,
                post_process_requested
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                format!("handy-{}.wav", timestamp),
                timestamp,
                false,
                format!("Recording {}", timestamp),
                text,
                post_processed,
                Option::<String>::None,
                false,
            ],
        )
        .expect("insert history entry");
    }

    fn user_version(conn: &Connection) -> i32 {
        conn.pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("read user_version")
    }

    #[test]
    fn migrating_adds_columns_without_raising_the_version() {
        let mut conn = Connection::open_in_memory().expect("open in-memory db");
        HistoryManager::apply_migrations(&mut conn).expect("migrate");
        for table in ["transcription_history", "binned_entries"] {
            for column in ADDED_COLUMNS {
                assert!(HistoryManager::has_column(&conn, table, column).expect("read table info"));
            }
        }
        assert_eq!(user_version(&conn), MIGRATIONS.len() as i32);

        // Opening it again changes nothing.
        HistoryManager::apply_migrations(&mut conn).expect("migrate again");
        assert_eq!(user_version(&conn), MIGRATIONS.len() as i32);
    }

    #[test]
    fn a_database_from_a_newer_handy_still_opens() {
        let mut conn = Connection::open_in_memory().expect("open in-memory db");
        HistoryManager::apply_migrations(&mut conn).expect("migrate");
        conn.execute(
            "ALTER TABLE transcription_history ADD COLUMN from_the_future TEXT",
            [],
        )
        .expect("add newer column");
        conn.pragma_update(None, "user_version", MIGRATIONS.len() as i32 + 3)
            .expect("set newer version");

        HistoryManager::apply_migrations(&mut conn).expect("open newer database");
        insert_entry(&conn, 100, "still works", None);
        let entry = HistoryManager::get_latest_entry_with_conn(&conn)
            .expect("fetch latest entry")
            .expect("entry exists");
        assert_eq!(entry.transcription_text, "still works");
    }

    #[test]
    fn reads_the_language_and_ignores_an_unknown_source() {
        let conn = setup_conn();
        let latest = |source: &str| {
            conn.execute(
                "INSERT INTO transcription_history (file_name, timestamp, title, transcription_text, language, language_source)
                 VALUES ('a.wav', (SELECT COUNT(*) FROM transcription_history), 'a', 'hallo', 'de', ?1)",
                [source],
            )
            .expect("insert entry");
            HistoryManager::get_latest_entry_with_conn(&conn)
                .unwrap()
                .unwrap()
        };
        let entry = latest("detected");
        assert_eq!(entry.language.as_deref(), Some("de"));
        assert_eq!(entry.language_source, Some(LanguageSource::Detected));
        assert_eq!(latest("from_the_future").language_source, None);
    }

    #[test]
    fn get_latest_entry_returns_none_when_empty() {
        let conn = setup_conn();
        let entry = HistoryManager::get_latest_entry_with_conn(&conn).expect("fetch latest entry");
        assert!(entry.is_none());
    }

    #[test]
    fn get_latest_entry_returns_newest_entry() {
        let conn = setup_conn();
        insert_entry(&conn, 100, "first", None);
        insert_entry(&conn, 200, "second", Some("processed"));

        let entry = HistoryManager::get_latest_entry_with_conn(&conn)
            .expect("fetch latest entry")
            .expect("entry exists");

        assert_eq!(entry.timestamp, 200);
        assert_eq!(entry.transcription_text, "second");
        assert_eq!(entry.post_processed_text.as_deref(), Some("processed"));
    }

    #[test]
    fn get_latest_completed_entry_skips_empty_entries() {
        let conn = setup_conn();
        insert_entry(&conn, 100, "completed", None);
        insert_entry(&conn, 200, "", None);

        let entry = HistoryManager::get_latest_completed_entry_with_conn(&conn)
            .expect("fetch latest completed entry")
            .expect("completed entry exists");

        assert_eq!(entry.timestamp, 100);
        assert_eq!(entry.transcription_text, "completed");
    }

    #[test]
    fn get_latest_completed_entry_skips_files() {
        let conn = setup_conn();
        insert_entry(&conn, 100, "dictated", None);
        conn.execute(
            "INSERT INTO transcription_history (file_name, timestamp, saved, title, transcription_text, source_name)
             VALUES ('handy-file-1.wav', 200, 1, 'file', 'from a file', 'talk.mp3')",
            [],
        )
        .expect("insert file entry");

        let entry = HistoryManager::get_latest_completed_entry_with_conn(&conn)
            .expect("fetch latest completed entry")
            .expect("completed entry exists");

        assert_eq!(entry.transcription_text, "dictated");
    }

    fn migrated_conn() -> Connection {
        let mut conn = Connection::open_in_memory().expect("open in-memory db");
        HistoryManager::apply_migrations(&mut conn).expect("migrate");
        conn
    }

    #[test]
    fn a_binned_entry_holds_its_binned_recording_and_comes_back_as_it_was() {
        let mut conn = migrated_conn();
        insert_entry(&conn, 100, "said", Some("polished"));

        // The recording goes first; the entry stays without it.
        let updated = HistoryManager::bin_recordings_with_conn(&mut conn, &[1], 500).unwrap();
        assert_eq!(updated[0].file_name, "");
        let items = HistoryManager::bin_items_with_conn(&conn).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, BinItemKind::Recording);
        assert_eq!(items[0].recording, "handy-100.wav");

        // Then the entry: one item, holding the recording again.
        assert_eq!(
            HistoryManager::bin_entries_with_conn(&mut conn, &[1], 600).unwrap(),
            vec![1]
        );
        assert!(HistoryManager::get_latest_entry_with_conn(&conn)
            .unwrap()
            .is_none());
        let items = HistoryManager::bin_items_with_conn(&conn).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, BinItemKind::Entry);
        assert_eq!(items[0].recording, "handy-100.wav");
        assert_eq!(items[0].binned_at, 600);

        // Restored, it has its id and text back, and the recording is in the
        // Bin again, as it was.
        let entry = HistoryManager::restore_entry_with_conn(&mut conn, 1).unwrap();
        assert_eq!(entry.id, 1);
        assert_eq!(entry.file_name, "");
        assert_eq!(entry.transcription_text, "said");
        assert_eq!(entry.post_processed_text.as_deref(), Some("polished"));
        let items = HistoryManager::bin_items_with_conn(&conn).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, BinItemKind::Recording);
        assert_eq!(items[0].recording, "handy-100.wav");
        assert_eq!(items[0].binned_at, 500);
        assert!(HistoryManager::restore_entry_with_conn(&mut conn, 1).is_err());

        // An entry binned with its own recording brings it back.
        HistoryManager::restore_recording_with_conn(&mut conn, 1).unwrap();
        HistoryManager::bin_entries_with_conn(&mut conn, &[1], 700).unwrap();
        let entry = HistoryManager::restore_entry_with_conn(&mut conn, 1).unwrap();
        assert_eq!(entry.file_name, "handy-100.wav");
        assert!(HistoryManager::bin_items_with_conn(&conn)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_binned_recording_returns_to_its_entry() {
        let mut conn = migrated_conn();
        insert_entry(&conn, 100, "said", None);
        HistoryManager::bin_recordings_with_conn(&mut conn, &[1], 500).unwrap();

        let entry = HistoryManager::restore_recording_with_conn(&mut conn, 1).unwrap();
        assert_eq!(entry.file_name, "handy-100.wav");
        assert!(HistoryManager::bin_items_with_conn(&conn)
            .unwrap()
            .is_empty());
        assert!(HistoryManager::restore_recording_with_conn(&mut conn, 1).is_err());
    }

    #[test]
    fn recordings_to_bin_skip_starred_in_use_newer_and_missing() {
        let conn = migrated_conn();
        for timestamp in [100, 200, 300, 400, 900] {
            insert_entry(&conn, timestamp, "said", None);
        }
        conn.execute(
            "UPDATE transcription_history SET saved = 1 WHERE timestamp = 200",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE transcription_history SET file_name = '' WHERE timestamp = 400",
            [],
        )
        .unwrap();

        let in_use = |id: i64| id == 3;
        assert_eq!(
            HistoryManager::recordings_to_bin(&conn, Some(500), in_use).unwrap(),
            vec![1]
        );
        assert_eq!(
            HistoryManager::recordings_to_bin(&conn, None, in_use).unwrap(),
            vec![1, 5]
        );
    }
}
