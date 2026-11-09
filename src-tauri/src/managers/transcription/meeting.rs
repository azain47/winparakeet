//! A meeting's tracks transcribed with timing: each WAV is streamed into the
//! chunker a piece at a time (an hour of both sides is never held in memory),
//! and every word keeps when it was said, in ms from the start of the track.

use anyhow::{anyhow, Result};
use hound::{SampleFormat, WavReader};
use log::{debug, info, warn};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Instant;
use transcribe_cpp::Transcript;

use super::{
    agree_on, engine_error, post_process_transcription_text, real_time_factor, ChunkControl,
    RunSetup, TranscriptionManager,
};
use crate::audio_toolkit::chunking::Chunker;
use crate::audio_toolkit::OutputLanguageEvidence;
use crate::engine_supervisor::{EngineError, LoadedInfo};
use crate::managers::audio::create_detector;
use crate::settings::{get_settings, AppSettings};

/// Tries per chunk when a dictation's cancel interrupts it.
const MAX_CHUNK_ATTEMPTS: usize = 3;

/// Audio read from a track and handed to the chunker at a time.
const FEED_SAMPLES: usize = 60 * 16_000;

const SAMPLES_PER_MS: usize = 16;

/// Something said, timed from the start of its track: a word, or a segment
/// or whole chunk when the model gives nothing finer.
#[derive(Clone, Debug, PartialEq)]
pub struct TimedUnit {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

/// One track's units, and how its text is finished.
pub struct TrackTranscript {
    pub units: Vec<TimedUnit>,
    settings: AppSettings,
    setup: RunSetup,
    output_language: OutputLanguageEvidence,
}

impl TrackTranscript {
    /// Post-processing for a piece of this track's text (custom words,
    /// filler removal, Chinese script), as a dictation's text gets.
    pub fn polish(&self, text: String) -> String {
        post_process_transcription_text(
            text,
            &self.settings,
            self.setup.custom_words_prompted(),
            &self.output_language,
            &self.setup.languages,
        )
    }
}

/// Every track's units, in the order the tracks were given.
pub struct TimedTranscription {
    pub tracks: Vec<TrackTranscript>,
    /// The language spoken, when known.
    pub language: Option<String>,
}

impl TranscriptionManager {
    /// Transcribes each 16 kHz mono 16-bit WAV in `tracks` with `model_id`,
    /// which is loaded for the job when another model is (see
    /// [`ensure_job_model`](Self::ensure_job_model)). Progress counts the
    /// samples of all tracks. `None` if `control` stopped it.
    pub fn transcribe_tracks(
        &self,
        model_id: &str,
        tracks: &[PathBuf],
        control: &dyn ChunkControl,
    ) -> Result<Option<TimedTranscription>> {
        let started = Instant::now();
        let readers = tracks
            .iter()
            .map(|path| open_track(path))
            .collect::<Result<Vec<_>>>()?;
        let total: usize = readers.iter().map(|r| r.len() as usize).sum();
        let settings = get_settings(&self.app_handle);

        let mut done = 0;
        control.progress(done, total);
        let mut results = Vec::with_capacity(readers.len());
        let mut languages = Vec::new();
        for reader in readers {
            let Some((track, track_languages)) =
                self.transcribe_track(model_id, reader, &settings, control, &mut done, total)?
            else {
                return Ok(None);
            };
            languages.extend(track_languages);
            results.push(track);
        }

        let language = results.first().and_then(|t| {
            t.setup
                .spoken_language(agree_on(languages).as_deref())
                .language
        });
        let elapsed_secs = started.elapsed().as_secs_f64();
        let audio_secs = total as f64 / 16_000.0;
        info!(
            "Meeting transcription of {:.1}s of audio in {} tracks with '{}' took {:.1}s ({:.1}x real time)",
            audio_secs,
            results.len(),
            model_id,
            elapsed_secs,
            real_time_factor(audio_secs, elapsed_secs)
        );
        Ok(Some(TimedTranscription {
            tracks: results,
            language,
        }))
    }

    /// One track, chunk by chunk; also the language each chunk's run
    /// reported.
    fn transcribe_track(
        &self,
        model_id: &str,
        mut reader: WavReader<BufReader<File>>,
        settings: &AppSettings,
        control: &dyn ChunkControl,
        done: &mut usize,
        total: usize,
    ) -> Result<Option<(TrackTranscript, Vec<Option<String>>)>> {
        let mut samples = reader.samples::<i16>();
        let head = read_piece(&mut samples);

        // The setup (language included) is decided once per track, from its
        // first minute, with the job's model.
        let Some(info) = self.job_model_info(model_id, control)? else {
            return Ok(None);
        };
        let plan = self.language_plan(settings, model_id, &info);
        let setup = self.setup_for(&info, settings, &plan, &head);

        let backend = settings.vad_backend;
        let app_handle = self.app_handle.clone();
        let mut chunker = Chunker::new(Box::new(move || create_detector(backend, &app_handle)));

        let mut units = Vec::new();
        let mut languages = Vec::new();
        // Chunks are contiguous: each starts where the one before ended.
        let mut offset = 0usize;
        let mut pending = Some(head);
        loop {
            // Scoring waits out dictation too, and stops on cancel.
            if !control.proceed() {
                return Ok(None);
            }
            let (chunks, last) = match pending.take() {
                Some(piece) if !piece.is_empty() => (chunker.push(piece), false),
                _ => (chunker.finish(), true),
            };
            for chunk in chunks {
                let len = chunk.len();
                let Some(transcript) = self.run_job_chunk(model_id, chunk, &setup, control)? else {
                    return Ok(None);
                };
                let offset_ms = (offset / SAMPLES_PER_MS) as i64;
                let chunk_ms = (len / SAMPLES_PER_MS) as i64;
                units.extend(timed_units(&transcript, offset_ms, chunk_ms));
                languages.push(transcript.language);
                offset += len;
                *done += len;
                control.progress(*done, total);
            }
            if last {
                break;
            }
            pending = Some(read_piece(&mut samples));
        }
        debug!(
            "Transcribed a meeting track: {} units over {:.1}s",
            units.len(),
            offset as f64 / 16_000.0
        );

        let output_language = setup.output_language(settings, agree_on(languages.clone()));
        Ok(Some((
            TrackTranscript {
                units,
                settings: settings.clone(),
                setup,
                output_language,
            },
            languages,
        )))
    }

    /// The job's model, loaded; `None` if `control` stopped the job.
    fn job_model_info(
        &self,
        model_id: &str,
        control: &dyn ChunkControl,
    ) -> Result<Option<LoadedInfo>> {
        loop {
            if !control.proceed() {
                return Ok(None);
            }
            if !self.ensure_job_model(model_id)? {
                continue;
            }
            match self.engine.loaded_model() {
                Some((id, info)) if id == model_id => return Ok(Some(info)),
                // A dictation's load came in between; wait it out.
                _ => continue,
            }
        }
    }

    /// One chunk on the job's model. Before each try the model is made the
    /// job's again (a dictation in between loads the selected one), and a
    /// run during which any load began is run again: it may have run on
    /// another model. `None` if `control` stopped the job.
    fn run_job_chunk(
        &self,
        model_id: &str,
        chunk: Vec<f32>,
        setup: &RunSetup,
        control: &dyn ChunkControl,
    ) -> Result<Option<Transcript>> {
        let mut attempts = 0;
        loop {
            if !control.proceed() {
                return Ok(None);
            }
            if !self.ensure_job_model(model_id)? {
                continue;
            }
            let loads = self.loads_begun.load(Ordering::SeqCst);
            if self.engine.loaded_model_id().as_deref() != Some(model_id) {
                continue;
            }
            attempts += 1;
            // A fresh mark per try: a dictation's cancel stops only the run
            // it caught, which is then tried again.
            let mark = self.engine.cancel_mark();
            match self.run_batch(mark, chunk.clone(), &setup.run_options, model_id) {
                Ok(transcript) if self.loads_begun.load(Ordering::SeqCst) == loads => {
                    return Ok(Some(transcript));
                }
                Ok(_) => {
                    attempts -= 1;
                    debug!("A model load began during a meeting chunk; running it again");
                }
                Err(EngineError::Cancelled) if attempts < MAX_CHUNK_ATTEMPTS => {
                    warn!("A meeting chunk was cancelled; trying it again");
                }
                Err(e) => return Err(engine_error(e)),
            }
        }
    }
}

/// Opens a track, which must be 16 kHz mono 16-bit PCM.
fn open_track(path: &Path) -> Result<WavReader<BufReader<File>>> {
    let reader =
        WavReader::open(path).map_err(|e| anyhow!("Failed to open {}: {}", path.display(), e))?;
    let spec = reader.spec();
    if spec.sample_rate != 16_000
        || spec.channels != 1
        || spec.bits_per_sample != 16
        || spec.sample_format != SampleFormat::Int
    {
        return Err(anyhow!("{} is not 16 kHz mono 16-bit PCM", path.display()));
    }
    Ok(reader)
}

/// The next piece of a track. A damaged tail (a crash mid-write) ends the
/// track instead of failing it.
fn read_piece(samples: &mut impl Iterator<Item = hound::Result<i16>>) -> Vec<f32> {
    let mut piece = Vec::with_capacity(FEED_SAMPLES);
    for sample in samples.take(FEED_SAMPLES) {
        match sample {
            Ok(sample) => piece.push(sample as f32 / 32_768.0),
            Err(e) => {
                warn!("A meeting track ends early: {}", e);
                break;
            }
        }
    }
    piece
}

/// A chunk's transcript as units timed from the start of the track: its
/// words, else its timed segments, else its whole text over the chunk.
fn timed_units(transcript: &Transcript, offset_ms: i64, chunk_ms: i64) -> Vec<TimedUnit> {
    let unit = |start: i64, end: i64, text: &str| {
        let text = text.trim();
        (!text.is_empty()).then(|| TimedUnit {
            start_ms: offset_ms + start.clamp(0, chunk_ms),
            end_ms: offset_ms + end.clamp(start.clamp(0, chunk_ms), chunk_ms),
            text: text.to_string(),
        })
    };
    if !transcript.words.is_empty() {
        return transcript
            .words
            .iter()
            .filter_map(|w| unit(w.t0_ms, w.t1_ms, &w.text))
            .collect();
    }
    if transcript.segments.iter().any(|s| s.t1_ms > 0) {
        return transcript
            .segments
            .iter()
            .filter_map(|s| unit(s.t0_ms, s.t1_ms, &s.text))
            .collect();
    }
    unit(0, chunk_ms, &transcript.text).into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use transcribe_cpp::{Segment, Word};

    #[test]
    fn units_are_words_then_segments_then_the_chunk() {
        let mut transcript = Transcript {
            text: "hello there".into(),
            ..Default::default()
        };
        assert_eq!(
            timed_units(&transcript, 1_000, 5_000),
            vec![TimedUnit {
                start_ms: 1_000,
                end_ms: 6_000,
                text: "hello there".into()
            }]
        );

        transcript.segments = vec![Segment {
            t0_ms: 100,
            t1_ms: 900,
            text: " hello there".into(),
            ..Default::default()
        }];
        assert_eq!(
            timed_units(&transcript, 1_000, 5_000)[0],
            TimedUnit {
                start_ms: 1_100,
                end_ms: 1_900,
                text: "hello there".into()
            }
        );

        transcript.words = vec![
            Word {
                t0_ms: 100,
                t1_ms: 400,
                text: " hello".into(),
                ..Default::default()
            },
            Word {
                t0_ms: 500,
                t1_ms: 9_000,
                text: " there".into(),
                ..Default::default()
            },
        ];
        let units = timed_units(&transcript, 1_000, 5_000);
        assert_eq!(units.len(), 2);
        assert_eq!(units[1].start_ms, 1_500);
        // Clamped to the chunk.
        assert_eq!(units[1].end_ms, 6_000);
    }
}
