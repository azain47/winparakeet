//! Background transcriptions (a file, an entry transcribed again, or a
//! meeting), one at a time in chunks that yield to dictation. Progress is
//! kept in memory.

use anyhow::{anyhow, Result};
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::{HashSet, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tauri_specta::Event;

use crate::actions::process_transcription_output;
use crate::audio_toolkit::{decode_audio_file, read_wav_samples, save_wav_file};
use crate::managers::history::{HistoryEntry, HistoryManager};
use crate::managers::meeting::capture::{MICROPHONE_TRACK, SYSTEM_TRACK};
use crate::managers::meeting::diarize::diarize;
use crate::managers::meeting::transcribe::{utterances, ME};
use crate::managers::meeting::{MeetingAudio, MeetingSegment, MeetingStatus, MeetingStore};
use crate::managers::model::ModelManager;
use crate::managers::transcription::{
    panic_payload_message, ChunkControl, Transcription, TranscriptionManager,
};
use crate::notices::{self, Notice, NoticeCode, NoticeSource};
use crate::settings::{get_settings, MeetingAudioRetention};

/// How often a waiting chunk checks whether dictation has finished.
const DICTATION_POLL: Duration = Duration::from_millis(200);

/// The share of a meeting's progress its transcription takes when the other
/// side is diarized after it.
const TRANSCRIBE_SHARE_WITH_DIARIZATION: usize = 90;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TranscriptionJobStatus {
    /// Waiting behind another job.
    Queued,
    /// `percent` of the audio is transcribed.
    Running {
        percent: u8,
    },
    /// The entry has its new text.
    Finished,
    /// Stopped before it finished; the entry keeps the text it had.
    Cancelled,
    Failed {
        error: String,
    },
}

/// A background transcription of one History entry.
#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct TranscriptionJob {
    pub entry_id: i64,
    pub status: TranscriptionJobStatus,
}

/// What a job transcribes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Entry(i64),
    Meeting(i64),
}

/// A job taken from the queues.
enum Job {
    Entry(i64),
    /// `model` is the one asked for; `None` uses the setting.
    Meeting {
        id: i64,
        model: Option<String>,
    },
}

struct Running {
    target: Target,
    cancelled: Arc<AtomicBool>,
    /// The last percent reported.
    percent: Arc<AtomicU8>,
}

#[derive(Default)]
struct Jobs {
    /// Entry ids waiting to be transcribed again. They go before files:
    /// someone is waiting on them.
    retries: VecDeque<i64>,
    /// Meetings waiting to be transcribed, with the model asked for. After
    /// retries, before files.
    meetings: VecDeque<(i64, Option<String>)>,
    /// Entry ids of files waiting to be transcribed.
    files: VecDeque<i64>,
    running: Option<Running>,
    /// Entries whose recording was made only for their job: files added
    /// while recordings aren't kept. It goes to the Bin when their last job
    /// ends.
    temporary: HashSet<i64>,
}

impl Jobs {
    fn has(&self, entry_id: i64) -> bool {
        self.running
            .as_ref()
            .is_some_and(|r| r.target == Target::Entry(entry_id))
            || self.retries.contains(&entry_id)
            || self.files.contains(&entry_id)
    }

    fn has_meeting(&self, id: i64) -> bool {
        self.running
            .as_ref()
            .is_some_and(|r| r.target == Target::Meeting(id))
            || self.meetings.iter().any(|(queued, _)| *queued == id)
    }

    fn is_idle(&self) -> bool {
        self.running.is_none()
            && self.retries.is_empty()
            && self.meetings.is_empty()
            && self.files.is_empty()
    }

    /// Drops the entry from the queues; whether it was in one.
    fn unqueue(&mut self, entry_id: i64) -> bool {
        let before = self.retries.len() + self.files.len();
        self.retries.retain(|&id| id != entry_id);
        self.files.retain(|&id| id != entry_id);
        self.retries.len() + self.files.len() != before
    }

    /// Drops the meeting from the queue; whether it was in it.
    fn unqueue_meeting(&mut self, id: i64) -> bool {
        let before = self.meetings.len();
        self.meetings.retain(|(queued, _)| *queued != id);
        self.meetings.len() != before
    }

    /// Asks the running job to stop if it is `target`'s.
    fn stop(&self, target: Target) {
        if let Some(running) = self.running.as_ref().filter(|r| r.target == target) {
            running.cancelled.store(true, Ordering::Release);
        }
    }
}

pub struct TranscriptionJobManager {
    app: AppHandle,
    jobs: Mutex<Jobs>,
    wake: Condvar,
}

impl TranscriptionJobManager {
    /// Starts the thread that runs the jobs.
    pub fn new(app: &AppHandle) -> Arc<Self> {
        let manager = Arc::new(Self {
            app: app.clone(),
            jobs: Mutex::new(Jobs::default()),
            wake: Condvar::new(),
        });
        let runner = Arc::clone(&manager);
        thread::Builder::new()
            .name("transcription-jobs".into())
            .spawn(move || runner.run_jobs())
            .expect("Failed to start the transcription job thread");
        manager
    }

    fn lock(&self) -> MutexGuard<'_, Jobs> {
        self.jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Decodes `path` into a recording, then adds it to History and queues
    /// it. The recording exists first, so no entry is ever without audio
    /// before its job ends.
    pub fn transcribe_file(&self, path: PathBuf) -> Result<HistoryEntry> {
        // A malformed file may panic the decoder.
        let samples = catch_unwind(AssertUnwindSafe(|| decode_audio_file(&path))).unwrap_or_else(
            |payload| {
                Err(anyhow!(
                    "Failed to read the file: {}",
                    panic_payload_message(payload.as_ref())
                ))
            },
        )?;
        if samples.is_empty() {
            return Err(anyhow!("The file has no audio"));
        }

        let hm = self.app.state::<Arc<HistoryManager>>();
        let file_name = format!("handy-file-{}.wav", chrono::Utc::now().timestamp_millis());
        let recording = hm.get_audio_file_path(&file_name);
        // Written aside and renamed, so a crash never leaves a partial one.
        let partial = recording.with_extension("wav.partial");
        if let Err(e) = save_wav_file(&partial, &samples)
            .and_then(|()| std::fs::rename(&partial, &recording).map_err(Into::into))
        {
            remove_recording(&partial);
            return Err(e);
        }
        drop(samples);

        let source_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let entry = {
            // Under the lock from the entry to its job, so `bin_recordings`
            // never takes the recording in between.
            let mut jobs = self.lock();
            let entry = match hm.save_file_entry(file_name, source_name) {
                Ok(entry) => entry,
                Err(e) => {
                    remove_recording(&recording);
                    return Err(e);
                }
            };
            if !crate::settings::get_recording_retention_period(&self.app).keeps_recordings() {
                jobs.temporary.insert(entry.id);
            }
            self.enqueue(&mut jobs, entry.id, false);
            entry
        };
        info!("Queued file transcription for history entry {}", entry.id);
        self.queued();
        Ok(entry)
    }

    /// Queues the entry to be transcribed again from its recording, ahead
    /// of any file, unless it already is.
    pub fn retry(&self, entry_id: i64) {
        if self.enqueue(&mut self.lock(), entry_id, true) {
            self.queued();
        }
    }

    /// Queues the entry unless it already is; whether it was queued. Its
    /// `Queued` goes out under the lock, before the job can be reported
    /// as running.
    fn enqueue(&self, jobs: &mut Jobs, entry_id: i64, retry: bool) -> bool {
        if jobs.has(entry_id) {
            return false;
        }
        if retry {
            jobs.retries.push_back(entry_id);
        } else {
            jobs.files.push_back(entry_id);
        }
        self.sync_job_active(jobs);
        self.emit(entry_id, TranscriptionJobStatus::Queued);
        true
    }

    /// After a job is queued, with the lock released.
    fn queued(&self) {
        crate::tray::update_tray_menu(&self.app);
        self.wake.notify_one();
    }

    /// The queued and running jobs, for a window that opens while they run.
    pub fn active_jobs(&self) -> Vec<TranscriptionJob> {
        let jobs = self.lock();
        let running = jobs
            .running
            .iter()
            .filter_map(|running| match running.target {
                Target::Entry(entry_id) => Some(TranscriptionJob {
                    entry_id,
                    status: TranscriptionJobStatus::Running {
                        percent: running.percent.load(Ordering::Relaxed),
                    },
                }),
                Target::Meeting(_) => None,
            });
        let queued = jobs
            .retries
            .iter()
            .chain(&jobs.files)
            .map(|&entry_id| TranscriptionJob {
                entry_id,
                status: TranscriptionJobStatus::Queued,
            });
        running.chain(queued).collect()
    }

    /// How many recordings `bin_recordings` would move without a cutoff.
    pub fn count_recordings(&self) -> Result<u32> {
        let jobs = self.lock();
        self.app
            .state::<Arc<HistoryManager>>()
            .count_recordings(|entry_id| jobs.has(entry_id))
    }

    /// Moves the recordings of entries that aren't starred to the Bin, all
    /// or those older than `older_than_days`; their text stays in History.
    /// A job's recording stays: this runs under the lock that queues jobs.
    pub fn bin_recordings(&self, older_than_days: Option<u32>) -> Result<u32> {
        let jobs = self.lock();
        self.app
            .state::<Arc<HistoryManager>>()
            .bin_recordings(older_than_days, |entry_id| jobs.has(entry_id))
    }

    /// After a job ends or leaves the queue: a recording made only for the
    /// entry's jobs goes to the Bin once none is left, unless recordings are
    /// kept by now.
    fn bin_temporary_recording(&self, jobs: &mut Jobs, entry_id: i64) {
        if jobs.has(entry_id) || !jobs.temporary.remove(&entry_id) {
            return;
        }
        if crate::settings::get_recording_retention_period(&self.app).keeps_recordings() {
            return;
        }
        if let Err(e) = self
            .app
            .state::<Arc<HistoryManager>>()
            .bin_recording(entry_id)
        {
            error!(
                "Failed to move the recording of history entry {} to the Bin: {}",
                entry_id, e
            );
        }
    }

    /// Stops the entry's job: a queued one is dropped, a running one stops
    /// before its next chunk.
    pub fn cancel(&self, entry_id: i64) {
        let idle = {
            let mut jobs = self.lock();
            jobs.stop(Target::Entry(entry_id));
            // A running job reports its own end from the job loop; a queued
            // one never reaches it, so it is reported here.
            if !jobs.unqueue(entry_id) {
                return;
            }
            self.sync_job_active(&jobs);
            self.emit(entry_id, TranscriptionJobStatus::Cancelled);
            self.bin_temporary_recording(&mut jobs, entry_id);
            jobs.is_idle()
        };
        self.after_job(idle);
    }

    fn meetings(&self) -> Arc<MeetingStore> {
        Arc::clone(&self.app.state::<Arc<MeetingStore>>())
    }

    /// Queues the meeting's transcription (no-op if already queued/running). model None = setting.
    pub fn enqueue_meeting(&self, id: i64, model_id: Option<String>) -> Result<()> {
        let store = self.meetings();
        {
            let mut jobs = self.lock();
            if jobs.has_meeting(id) {
                return Ok(());
            }
            let meeting = store.get(id)?;
            if store.is_binned(id)? {
                return Err(anyhow!("The meeting is in the Bin"));
            }
            if meeting.status == MeetingStatus::Recording {
                return Err(anyhow!("The meeting is still recording"));
            }
            if meeting.audio != MeetingAudio::Kept {
                return Err(anyhow!(
                    "The meeting's audio is no longer kept, so it can't be transcribed"
                ));
            }
            jobs.meetings
                .push_back((id, model_id.filter(|m| !m.trim().is_empty())));
            self.sync_job_active(&jobs);
            // Under the lock, before the job can be reported as running.
            store.set_status(id, MeetingStatus::Queued, None)?;
        }
        info!("Queued the transcription of meeting {}", id);
        self.queued();
        Ok(())
    }

    /// Queues the meetings whose transcription was queued or running when
    /// the app last closed.
    pub fn resume_meetings(&self) {
        let meetings = match self.meetings().unfinished_transcriptions() {
            Ok(meetings) => meetings,
            Err(e) => {
                error!("Failed to read unfinished meeting transcriptions: {}", e);
                return;
            }
        };
        for meeting in meetings {
            if let Err(e) = self.enqueue_meeting(meeting.id, None) {
                warn!("Meeting {} can't be transcribed: {}", meeting.id, e);
                let _ = self.meetings().set_status(
                    meeting.id,
                    MeetingStatus::Failed,
                    Some(&e.to_string()),
                );
            }
        }
    }

    /// Stops the meeting's transcription: a queued one is dropped, a running
    /// one stops before its next chunk. Its transcript from before stays.
    pub fn cancel_meeting(&self, id: i64) {
        let idle = {
            let mut jobs = self.lock();
            if !self.stop_meeting(&mut jobs, id) {
                return;
            }
            jobs.is_idle()
        };
        self.after_job(idle);
    }

    /// Stops the meeting's job under the lock; whether a queued one was
    /// dropped (a running one reports its own end).
    fn stop_meeting(&self, jobs: &mut Jobs, id: i64) -> bool {
        jobs.stop(Target::Meeting(id));
        if !jobs.unqueue_meeting(id) {
            return false;
        }
        self.sync_job_active(jobs);
        self.meeting_cancelled(id);
        true
    }

    /// A cancelled meeting is back to `ready` if it has a transcript, else
    /// `recorded`.
    fn meeting_cancelled(&self, id: i64) {
        let store = self.meetings();
        let status = match store.has_transcript(id) {
            Ok(true) => MeetingStatus::Ready,
            Ok(false) => MeetingStatus::Recorded,
            Err(e) => {
                error!("{}", e);
                return;
            }
        };
        if let Err(e) = store.set_status(id, status, None) {
            error!("Failed to update meeting {}: {}", id, e);
        }
    }

    /// Moves the meeting (`audio_only`: only its audio) to the Bin, stopping
    /// its transcription first. Under the lock that queues jobs, so none
    /// starts on audio that is leaving.
    pub fn bin_meeting(&self, id: i64, audio_only: bool) -> Result<()> {
        let store = self.meetings();
        let idle = {
            let mut jobs = self.lock();
            self.stop_meeting(&mut jobs, id);
            if audio_only {
                store.bin_audio(id)?;
            } else {
                store.bin(id)?;
            }
            jobs.is_idle()
        };
        self.after_job(idle);
        Ok(())
    }

    /// How many meetings' audio `retention` would move to the Bin now.
    pub fn count_meeting_audio_to_bin(&self, retention: MeetingAudioRetention) -> Result<u32> {
        let jobs = self.lock();
        self.meetings()
            .count_audio_to_bin(retention, |id| jobs.has_meeting(id))
    }

    /// Moves the meeting audio the retention setting no longer keeps to the
    /// Bin. A job's audio stays: this runs under the lock that queues jobs.
    pub fn apply_meeting_retention(&self) {
        let retention = get_settings(&self.app).meeting_audio_retention;
        let jobs = self.lock();
        if let Err(e) = self
            .meetings()
            .apply_retention(retention, |id| jobs.has_meeting(id))
        {
            error!("Failed to apply the meeting audio retention: {}", e);
        }
    }

    /// After a job ended or left the queue, with the lock released.
    fn after_job(&self, idle: bool) {
        if idle {
            let tm = self.app.state::<Arc<TranscriptionManager>>();
            tm.release_job_model();
            // A dictation under way unloads the model itself when it ends.
            if !crate::tray::dictation_in_progress(&self.app) {
                tm.maybe_unload_immediately("background transcription");
            }
        }
        crate::tray::update_tray_menu(&self.app);
    }

    /// Keeps the model held for as long as any job is queued or running.
    fn sync_job_active(&self, jobs: &Jobs) {
        self.app
            .state::<Arc<TranscriptionManager>>()
            .set_job_active(!jobs.is_idle());
    }

    fn emit(&self, entry_id: i64, status: TranscriptionJobStatus) {
        if let Err(e) = (TranscriptionJob { entry_id, status }).emit(&self.app) {
            error!("Failed to emit transcription job event: {}", e);
        }
    }

    fn run_jobs(&self) {
        loop {
            let (job, cancelled, percent) = {
                let mut jobs = self.lock();
                let job = loop {
                    if let Some(entry_id) = jobs.retries.pop_front() {
                        break Job::Entry(entry_id);
                    }
                    if let Some((id, model)) = jobs.meetings.pop_front() {
                        break Job::Meeting { id, model };
                    }
                    if let Some(entry_id) = jobs.files.pop_front() {
                        break Job::Entry(entry_id);
                    }
                    jobs = self
                        .wake
                        .wait(jobs)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                };
                let cancelled = Arc::new(AtomicBool::new(false));
                let percent = Arc::new(AtomicU8::new(0));
                let target = match &job {
                    Job::Entry(entry_id) => Target::Entry(*entry_id),
                    Job::Meeting { id, .. } => Target::Meeting(*id),
                };
                jobs.running = Some(Running {
                    target,
                    cancelled: Arc::clone(&cancelled),
                    percent: Arc::clone(&percent),
                });
                // Under the lock, so it can't overtake the job's `Queued`.
                match target {
                    Target::Entry(entry_id) => {
                        self.emit(entry_id, TranscriptionJobStatus::Running { percent: 0 })
                    }
                    Target::Meeting(id) => {
                        if let Err(e) =
                            self.meetings()
                                .set_status(id, MeetingStatus::Transcribing, None)
                        {
                            error!("Failed to update meeting {}: {}", id, e);
                        }
                    }
                }
                (job, cancelled, percent)
            };

            match job {
                Job::Entry(entry_id) => self.run_entry_job(entry_id, &cancelled, &percent),
                Job::Meeting { id, model } => self.run_meeting_job(id, model, &cancelled, &percent),
            }
        }
    }

    /// Ends the running job: `report` runs under the lock, so a new job for
    /// the same entry or meeting can't be reported before this one's end.
    fn end_job(&self, report: impl FnOnce(&mut Jobs)) {
        let idle = {
            let mut jobs = self.lock();
            jobs.running = None;
            self.sync_job_active(&jobs);
            report(&mut jobs);
            jobs.is_idle()
        };
        if idle {
            self.after_job(true);
        }
    }

    fn run_entry_job(&self, entry_id: i64, cancelled: &AtomicBool, percent: &AtomicU8) {
        // A panic fails only this job; the rest of the loop still has to
        // run, or the model would stay held and the queue stuck.
        let job = || self.run_job(entry_id, cancelled, percent);
        let result = catch_unwind(AssertUnwindSafe(job))
            .unwrap_or_else(|payload| Err(crashed(payload.as_ref())));

        let status = match result {
            Err(_) | Ok(JobEnd::Cancelled) if cancelled.load(Ordering::Acquire) => {
                TranscriptionJobStatus::Cancelled
            }
            Err(e) => {
                warn!("Transcription of history entry {} failed: {}", entry_id, e);
                notices::raise(
                    &self.app,
                    Notice::new(NoticeCode::FileTranscriptionFailed, NoticeSource::File)
                        .with_detail(e.to_string()),
                );
                TranscriptionJobStatus::Failed {
                    error: e.to_string(),
                }
            }
            Ok(JobEnd::Finished {
                post_process_failed,
            }) => {
                // Clear first, so the warning about this job survives it.
                notices::clear(&self.app, NoticeSource::File);
                if post_process_failed {
                    notices::raise(
                        &self.app,
                        Notice::new(NoticeCode::FilePostProcessFailed, NoticeSource::File),
                    );
                }
                TranscriptionJobStatus::Finished
            }
            Ok(JobEnd::Cancelled) => TranscriptionJobStatus::Cancelled,
        };

        self.end_job(|jobs| {
            self.emit(entry_id, status);
            self.bin_temporary_recording(jobs, entry_id);
        });
    }

    fn run_meeting_job(
        &self,
        id: i64,
        model: Option<String>,
        cancelled: &AtomicBool,
        percent: &AtomicU8,
    ) {
        let job = || self.transcribe_meeting(id, model, cancelled, percent);
        let result = catch_unwind(AssertUnwindSafe(job))
            .unwrap_or_else(|payload| Err(crashed(payload.as_ref())));
        let finished = matches!(result, Ok(Some(_)));

        let store = self.meetings();
        self.end_job(|_| {
            let saved = match result {
                Ok(Some(transcript)) => store
                    .save_transcript(
                        id,
                        &transcript.segments,
                        &transcript.model_id,
                        transcript.language.as_deref(),
                    )
                    .map(|_| info!("Meeting {} transcribed", id)),
                Err(_) | Ok(None) if cancelled.load(Ordering::Acquire) => {
                    self.meeting_cancelled(id);
                    Ok(())
                }
                Ok(None) => {
                    self.meeting_cancelled(id);
                    Ok(())
                }
                Err(e) => {
                    warn!("Transcription of meeting {} failed: {}", id, e);
                    store
                        .set_status(id, MeetingStatus::Failed, Some(&e.to_string()))
                        .map(|_| ())
                }
            };
            if let Err(e) = saved {
                error!("Failed to update meeting {}: {}", id, e);
            }
        });
        if finished {
            self.apply_meeting_retention();
        }
    }

    /// Transcribes a meeting's tracks with its model, diarizes the other
    /// side, and builds the transcript. `None` if it was cancelled.
    fn transcribe_meeting(
        &self,
        id: i64,
        model: Option<String>,
        cancelled: &AtomicBool,
        percent: &AtomicU8,
    ) -> Result<Option<MeetingTranscript>> {
        let store = self.meetings();
        let tm = Arc::clone(&self.app.state::<Arc<TranscriptionManager>>());
        let model_manager = Arc::clone(&self.app.state::<Arc<ModelManager>>());
        let settings = get_settings(&self.app);

        let meeting = store.get(id)?;
        if meeting.audio != MeetingAudio::Kept {
            return Err(anyhow!(
                "The meeting's audio is no longer kept, so it can't be transcribed"
            ));
        }
        let model_id = model
            .or_else(|| settings.meeting_model.clone())
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| settings.selected_model.clone());
        if model_id.is_empty() {
            return Err(anyhow!("No transcription model is selected"));
        }
        match model_manager.get_model_info(&model_id) {
            Some(info) if info.is_downloaded => {}
            Some(info) => return Err(anyhow!("The model {} isn't downloaded", info.name)),
            None => return Err(anyhow!("The model {} isn't available", model_id)),
        }

        let folder = store.folder(id);
        let microphone = Some(folder.join(MICROPHONE_TRACK))
            .filter(|path| meeting.has_microphone && path.is_file());
        let system = Some(folder.join(SYSTEM_TRACK))
            .filter(|path| meeting.has_system_audio && path.is_file());
        let paths: Vec<PathBuf> = microphone.iter().chain(system.iter()).cloned().collect();
        if paths.is_empty() {
            return Err(anyhow!("The meeting has no recorded audio"));
        }
        let diarizer = system
            .as_ref()
            .filter(|_| settings.meeting_speaker_labels)
            .and_then(|_| model_manager.diarizer_path());

        let control = Control {
            manager: self,
            target: Target::Meeting(id),
            cancelled,
            percent,
            share: if diarizer.is_some() {
                TRANSCRIBE_SHARE_WITH_DIARIZATION
            } else {
                100
            },
        };
        let Some(timed) = tm.transcribe_tracks(&model_id, &paths, &control)? else {
            return Ok(None);
        };
        let mut tracks = timed.tracks.into_iter();
        let mine = microphone.as_ref().and_then(|_| tracks.next());
        let theirs = system.as_ref().and_then(|_| tracks.next());

        // Diarization only when the other side said something; a failure
        // leaves it one speaker.
        let mut turns = Vec::new();
        if let (Some(diarizer), Some(system), Some(theirs)) = (&diarizer, &system, &theirs) {
            if !theirs.units.is_empty() {
                match diarize(diarizer, system, cancelled) {
                    Ok(Some(found)) => turns = found,
                    Ok(None) => return Ok(None),
                    Err(e) => warn!("Meeting {}: {:#}; the other side stays one speaker", id, e),
                }
            }
        }
        if cancelled.load(Ordering::Acquire) {
            return Ok(None);
        }

        let segments: Vec<MeetingSegment> = utterances(
            mine.as_ref().map_or(&[][..], |t| t.units.as_slice()),
            theirs.as_ref().map_or(&[][..], |t| t.units.as_slice()),
            &turns,
        )
        .into_iter()
        .filter_map(|mut segment| {
            let track = if segment.speaker == ME {
                mine.as_ref()
            } else {
                theirs.as_ref()
            }?;
            segment.text = track.polish(segment.text);
            (!segment.text.trim().is_empty()).then_some(segment)
        })
        .collect();
        if segments.is_empty() {
            return Err(anyhow!("No speech was found in the meeting's audio"));
        }
        Ok(Some(MeetingTranscript {
            segments,
            model_id,
            language: timed.language,
        }))
    }

    fn run_job(&self, entry_id: i64, cancelled: &AtomicBool, percent: &AtomicU8) -> Result<JobEnd> {
        let hm = Arc::clone(&self.app.state::<Arc<HistoryManager>>());
        let tm = Arc::clone(&self.app.state::<Arc<TranscriptionManager>>());
        let entry = tauri::async_runtime::block_on(hm.get_entry_by_id(entry_id))?
            .ok_or_else(|| anyhow!("History entry {} not found", entry_id))?;
        if entry.file_name.is_empty() {
            return Err(anyhow!("The recording of this entry was not kept"));
        }
        let samples = read_wav_samples(hm.get_audio_file_path(&entry.file_name))
            .map_err(|e| anyhow!("Failed to load audio: {}", e))?;
        if samples.is_empty() {
            return Err(anyhow!("Recording has no audio samples"));
        }

        let control = Control {
            manager: self,
            target: Target::Entry(entry.id),
            cancelled,
            percent,
            share: 100,
        };
        let Some(Transcription { text, language }) = tm.transcribe_in_chunks(samples, &control)?
        else {
            return Ok(JobEnd::Cancelled);
        };
        if text.is_empty() {
            return Err(anyhow!("Recording contains no speech"));
        }
        if cancelled.load(Ordering::Acquire) {
            return Ok(JobEnd::Cancelled);
        }

        let processed = tauri::async_runtime::block_on(process_transcription_output(
            &self.app,
            &text,
            entry.post_process_requested,
        ));
        // Post-processing can take a while; a cancel during it still wins.
        if cancelled.load(Ordering::Acquire) {
            return Ok(JobEnd::Cancelled);
        }
        let post_process_failed = processed.post_process_failed;
        hm.update_transcription(
            entry.id,
            text,
            processed.post_processed_text,
            processed.post_process_prompt,
            language,
        )?;
        Ok(JobEnd::Finished {
            post_process_failed,
        })
    }
}

/// A cancelled job changes no notices.
enum JobEnd {
    Finished { post_process_failed: bool },
    Cancelled,
}

/// A meeting's new transcript.
struct MeetingTranscript {
    segments: Vec<MeetingSegment>,
    model_id: String,
    language: Option<String>,
}

/// Best-effort cleanup; the caller is already reporting the failure.
fn remove_recording(path: &Path) {
    if let Err(e) = std::fs::remove_file(path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            error!("Failed to remove {}: {}", path.display(), e);
        }
    }
}

fn crashed(payload: &(dyn std::any::Any + Send)) -> anyhow::Error {
    anyhow!("Transcription crashed: {}", panic_payload_message(payload))
}

struct Control<'a> {
    manager: &'a TranscriptionJobManager,
    target: Target,
    cancelled: &'a AtomicBool,
    /// The last percent reported; the job starts at 0.
    percent: &'a AtomicU8,
    /// The percent reached when all the audio is transcribed; a meeting's
    /// diarization takes the rest.
    share: usize,
}

impl ChunkControl for Control<'_> {
    fn proceed(&self) -> bool {
        // Dictation goes first: wait out a recording and its transcription.
        // A chunk started while a stream is open would end that stream.
        loop {
            if self.cancelled.load(Ordering::Acquire) {
                return false;
            }
            if !crate::tray::dictation_in_progress(&self.manager.app) {
                return true;
            }
            thread::sleep(DICTATION_POLL);
        }
    }

    fn progress(&self, done: usize, total: usize) {
        // The chunk a cancel caught still finishes; don't report it as running.
        if self.cancelled.load(Ordering::Acquire) {
            return;
        }
        let percent = (done * self.share / total.max(1)).min(100) as u8;
        if self.percent.swap(percent, Ordering::Relaxed) == percent {
            return;
        }
        match self.target {
            Target::Entry(entry_id) => self
                .manager
                .emit(entry_id, TranscriptionJobStatus::Running { percent }),
            Target::Meeting(id) => self.manager.meetings().set_progress(id, percent),
        }
    }
}
