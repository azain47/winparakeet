//! A recording's live decode: a stream the model decodes as audio arrives,
//! or long-form chunks decoded as the recording goes. With the Live overlay,
//! a model that can't stream gets a preview instead: the part of the
//! recording no chunk has decoded yet is decoded again as it grows.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use log::{debug, error, info};
use transcribe_cpp::{RunOptions, StreamOptions};

use super::chunked::{ChunkSetup, ChunkedDecode, PendingSetup};
use super::policy::LiveDecode;
use super::{
    emit_stream_text, join_chunks, lock_perf, run_setup, PreviewScript, RunOutcome, RunSetup,
    StreamFinalizeReply, StreamPerf, TranscriptionManager,
};
use crate::audio_toolkit::{chunking, OutputLanguageEvidence};
use crate::engine_supervisor::{CancelMark, EngineError, LoadedInfo, StreamHandle, StreamProgress};
use crate::language_detection::Plan;
use crate::settings::{get_settings, AppSettings, OverlayStyle};

/// Audio a preview needs past what the chunks decoded.
const PREVIEW_MIN_SAMPLES: usize = 16_000 * 6 / 10;
/// New audio a preview waits for after the last one.
const PREVIEW_STEP_SAMPLES: usize = 16_000 * 3 / 10;
/// The shortest gap between previews; after a slower decode, twice its time.
const PREVIEW_INTERVAL: Duration = Duration::from_millis(700);
/// Two decodes in a row slower than this end the preview: on this device it
/// would hold up the transcription when the recording stops.
const PREVIEW_SLOW: Duration = Duration::from_millis(1500);

/// One recording's live decode.
pub(super) struct LiveSession<'a> {
    decoder: LiveDecoder<'a>,
    /// For a model that can't stream, with the Live overlay.
    preview: Option<Preview<'a>>,
}

enum LiveDecoder<'a> {
    /// With the run setup its result finishes with.
    Stream(StreamSession<'a>, Box<RunSetup>),
    /// Its setup is resolved by its first chunk.
    Chunked(Box<ChunkedDecode<'a>>),
    /// Nothing is decoded live: the recording is transcribed in batch when it
    /// stops, and the session only previews it.
    Batch,
}

/// A decoded text and the language the model detected in it, if any.
type Decoded = (String, Option<String>);

impl<'a> LiveSession<'a> {
    /// Begins the live decode `mode` picks on the loaded model, previewed for
    /// the Live overlay when the model can't stream. `None` when there is
    /// nothing to do live or it couldn't begin: the recording is then
    /// transcribed in batch when it stops.
    pub(super) fn begin(
        tm: &'a TranscriptionManager,
        mode: LiveDecode,
        info: &LoadedInfo,
        model_id: &str,
    ) -> Option<Self> {
        let settings = get_settings(&tm.app_handle);
        let previews = mode != LiveDecode::Stream && settings.overlay_style == OverlayStyle::Live;
        if mode == LiveDecode::None && !previews {
            return None;
        }
        // The same run setup as the batch path: task + language gated
        // against what the model actually advertises.
        let plan = tm.language_plan(&settings, model_id, info);
        let preview = previews.then(|| Preview::new(tm, info, &settings, &plan, model_id));
        let decoder = match mode {
            LiveDecode::None => LiveDecoder::Batch,
            LiveDecode::Stream => {
                // A stream's language is fixed before any audio, so it can't
                // be detected; a detect plan falls back.
                let setup = tm.setup_for(info, &settings, &plan, &[]);
                let stream = StreamSession::begin(tm, info, &settings, &setup, model_id)?;
                LiveDecoder::Stream(stream, Box::new(setup))
            }
            LiveDecode::Chunked => {
                info!("Live long-form chunking started (model '{}')", model_id);
                let backend = settings.vad_backend;
                LiveDecoder::Chunked(Box::new(ChunkedDecode::new(
                    tm,
                    tm.engine.cancel_mark(),
                    ChunkSetup::Pending(Box::new(PendingSetup {
                        plan,
                        info: info.clone(),
                        settings,
                    })),
                    model_id,
                    backend,
                )))
            }
        };
        if preview.is_some() {
            tm.stream_active.store(true, Ordering::Release);
            info!("Live preview started (model '{}')", model_id);
        }
        Some(Self { decoder, preview })
    }

    pub(super) fn feed(&mut self, pcm: Vec<f32>) {
        if let Some(preview) = &mut self.preview {
            preview.audio.extend_from_slice(&pcm);
        }
        match &mut self.decoder {
            LiveDecoder::Stream(stream, _) => stream.feed(pcm),
            LiveDecoder::Chunked(chunks) => chunks.push(pcm),
            LiveDecoder::Batch => {}
        }
    }

    /// Previews the recording so far, when one is due. Called once the
    /// worker has caught up with the audio, so it never holds up a stop that
    /// is already waiting.
    pub(super) fn preview(&mut self) {
        let Some(preview) = &mut self.preview else {
            return;
        };
        match &self.decoder {
            LiveDecoder::Chunked(chunks) if !chunks.failed() => {
                let decoded = chunks.decoded_samples();
                if preview.due(decoded) {
                    preview.run(decoded, &chunks.decoded_text(), chunks.run_options());
                }
            }
            LiveDecoder::Batch => {
                if preview.due(0) {
                    preview.run(0, "", None);
                }
            }
            LiveDecoder::Chunked(_) | LiveDecoder::Stream(..) => {}
        }
    }

    /// The live result; `None` to transcribe the recording in batch instead.
    /// `recorded_samples` is the whole recording's length.
    pub(super) fn finish(self, recorded_samples: usize) -> StreamFinalizeReply {
        match self.decoder {
            LiveDecoder::Stream(stream, setup) => {
                Ok(stream
                    .finish()?
                    .map(|(text, model_detected_language)| RunOutcome {
                        setup: *setup,
                        text,
                        model_detected_language,
                    }))
            }
            LiveDecoder::Chunked(chunks) => finish_chunks(*chunks, recorded_samples),
            // Transcribed in batch, as without a live session.
            LiveDecoder::Batch => Ok(None),
        }
    }
}

/// The Live overlay's text while recording with a model that can't stream:
/// the chunks decoded so far, then the rest of the recording decoded again as
/// it grows. Only the overlay sees it; the transcription is decoded as it
/// would be without it.
struct Preview<'a> {
    tm: &'a TranscriptionManager,
    mark: CancelMark,
    model_id: String,
    /// Until the first chunk sets the language, the plan's language before
    /// any speech is heard: a fixed one, or the model's own detection (the
    /// first preferred language for a model without it).
    setup: RunSetup,
    script: PreviewScript,
    /// The whole recording so far.
    audio: Vec<f32>,
    /// How much of it the last preview decoded.
    previewed: usize,
    next_at: Instant,
    /// Previews in a row slower than [`PREVIEW_SLOW`].
    slow: u8,
    stopped: bool,
}

impl<'a> Preview<'a> {
    fn new(
        tm: &'a TranscriptionManager,
        info: &LoadedInfo,
        settings: &AppSettings,
        plan: &Plan,
        model_id: &str,
    ) -> Self {
        let setup = run_setup(info, settings, plan.without_speech());
        let script = PreviewScript::new(
            settings.chinese_script,
            &setup.output_language(settings, None),
        );
        Self {
            tm,
            mark: tm.engine.cancel_mark(),
            model_id: model_id.to_string(),
            setup,
            script,
            audio: Vec::new(),
            previewed: 0,
            next_at: Instant::now(),
            slow: 0,
            stopped: false,
        }
    }

    /// Whether to decode again: enough audio past `decoded`, some of it new,
    /// and the last decode long enough ago.
    fn due(&self, decoded: usize) -> bool {
        !self.stopped
            && self.audio.len() >= decoded + PREVIEW_MIN_SAMPLES
            && self.audio.len() >= self.previewed + PREVIEW_STEP_SAMPLES
            && Instant::now() >= self.next_at
    }

    /// Decodes the audio past `decoded` and shows it after `decoded_text`.
    fn run(&mut self, decoded: usize, decoded_text: &str, run_options: Option<&RunOptions>) {
        self.previewed = self.audio.len();
        let tail = self.audio[decoded..].to_vec();
        let run_options = run_options.unwrap_or(&self.setup.run_options);
        let started = Instant::now();
        let transcript = match self
            .tm
            .run_batch(self.mark, tail, run_options, &self.model_id)
        {
            Ok(transcript) => transcript,
            Err(e) => {
                // Cancelled, or a failure the transcription itself will meet
                // and report.
                debug!("Live preview stopped: {}", e);
                self.stopped = true;
                return;
            }
        };
        let took = started.elapsed();

        // Joined as the final text joins its chunks, then split into the part
        // that is final and the part that may still change.
        let text = chunking::join_transcripts(&[decoded_text, transcript.text.as_str()]);
        let (committed, tentative) = match text.strip_prefix(decoded_text) {
            Some(rest) if !decoded_text.is_empty() => (decoded_text, rest),
            _ => ("", text.as_str()),
        };
        let (committed, tentative) =
            self.script
                .convert(committed, tentative, &self.setup.languages);
        emit_stream_text(&self.tm.app_handle, &committed, &tentative);

        self.slow = if took > PREVIEW_SLOW {
            self.slow + 1
        } else {
            0
        };
        if self.slow >= 2 {
            info!(
                "Live preview stopped: decoding took {:.1}s, so the text shows when the recording stops",
                took.as_secs_f64()
            );
            self.stopped = true;
        }
        self.next_at = Instant::now() + PREVIEW_INTERVAL.max(took * 2);
    }
}

fn finish_chunks(
    chunks: ChunkedDecode<'_>,
    recorded_samples: usize,
) -> Result<Option<RunOutcome>, EngineError> {
    // A failure stops the decode short of the recording; report the failure.
    if chunks.samples() != recorded_samples && !chunks.failed() {
        debug!(
            "Live chunks saw {} of the recording's {} samples; transcribing it in batch",
            chunks.samples(),
            recorded_samples
        );
        return Ok(None);
    }
    let while_recording = chunks.decoded();
    match chunks.finish() {
        Ok((transcripts, setup)) => {
            if while_recording > 0 {
                info!(
                    "{} of {} chunks were decoded while recording",
                    while_recording,
                    transcripts.len()
                );
            }
            let (text, model_detected_language) = join_chunks(transcripts);
            Ok(Some(RunOutcome {
                setup,
                text,
                model_detected_language,
            }))
        }
        Err(EngineError::Cancelled) => Err(EngineError::Cancelled),
        Err(e) => {
            info!("Live chunks failed ({}); transcribing in batch", e);
            Ok(None)
        }
    }
}

/// A stream the model decodes as audio arrives, with preview text emitted
/// to the overlay.
struct StreamSession<'a> {
    tm: &'a TranscriptionManager,
    model_id: String,
    stream: StreamHandle,
    perf: Arc<Mutex<StreamPerf>>,
    /// In auto mode the model's own LID is the best remaining evidence; the
    /// snapshot is only materialized when it can change the outcome.
    want_language: bool,
}

impl<'a> StreamSession<'a> {
    fn begin(
        tm: &'a TranscriptionManager,
        info: &LoadedInfo,
        settings: &AppSettings,
        setup: &RunSetup,
        model_id: &str,
    ) -> Option<Self> {
        let output_language = setup.output_language(settings, None);

        // Feed results arrive on the engine's thread; this callback only
        // converts and emits preview text.
        let perf = Arc::new(Mutex::new(StreamPerf::new()));
        let on_progress = {
            let perf = Arc::clone(&perf);
            let app_handle = tm.app_handle.clone();
            let languages = setup.languages.clone();
            let mut preview_script = PreviewScript::new(settings.chinese_script, &output_language);
            move |progress: StreamProgress| {
                let mut perf = lock_perf(&perf);
                perf.record_compute(progress.elapsed);
                perf.record_update(&progress.update);
                if let Some(text) = progress.text {
                    perf.record_emit();
                    let (committed, tentative) =
                        preview_script.convert(&text.committed, &text.tentative, &languages);
                    emit_stream_text(&app_handle, &committed, &tentative);
                }
                perf.maybe_log();
            }
        };

        // Run the stream in the engine's worker process. Feeds are queued
        // without waiting; a crashed or hung worker makes finalize report no
        // result, and the caller falls back to batch transcription.
        // StreamOptions::default() uses CommitPolicy::Auto and lets the
        // family pick its own streaming strategy (no family-specific ext).
        let stream = match tm.engine.start_stream(
            setup.run_options.clone(),
            StreamOptions::default(),
            on_progress,
        ) {
            Ok(stream) => stream,
            Err(e) => {
                error!("Failed to begin stream: {}", e);
                tm.forget_model_if_engine_dropped(model_id, &e.to_string());
                return None;
            }
        };

        tm.stream_active.store(true, Ordering::Release);
        info!(
            "Live streaming transcription started (model '{}', backend '{}')",
            model_id, info.backend
        );
        Some(Self {
            tm,
            model_id: model_id.to_string(),
            stream,
            perf,
            want_language: matches!(output_language, OutputLanguageEvidence::Unknown),
        })
    }

    fn feed(&mut self, pcm: Vec<f32>) {
        lock_perf(&self.perf).record_feed(pcm.len());
        self.stream.feed(pcm);
    }

    fn finish(self) -> Result<Option<Decoded>, EngineError> {
        let result = match self.stream.finalize(self.want_language) {
            // After finalize the committed prefix holds the full text;
            // display() = committed + tentative is the safe read.
            Ok(Some(finalized)) => {
                let mut perf = lock_perf(&self.perf);
                perf.record_compute(finalized.elapsed);
                perf.record_update(&finalized.update);
                if finalized.text.full.trim().is_empty() {
                    // A stream can miss what a run over the whole recording
                    // finds, so empty text retries in batch.
                    info!("Live transcription produced no text; retrying in batch");
                    Ok(None)
                } else {
                    Ok(Some((finalized.text.full, finalized.language)))
                }
            }
            Ok(None) => {
                error!(
                    "Live transcription could not be finalized; \
                     falling back to batch transcription"
                );
                self.tm.forget_model_if_engine_dropped(
                    &self.model_id,
                    "live transcription failed and the model could not be restarted",
                );
                Ok(None)
            }
            Err(e) => {
                info!("Live transcription finalize stopped: {}", e);
                Err(e)
            }
        };
        let chars = match &result {
            Ok(Some((text, _))) => text.len(),
            _ => 0,
        };
        lock_perf(&self.perf).log_finalized(chars);
        result
    }
}
