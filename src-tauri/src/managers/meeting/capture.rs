//! Records a meeting: the dictation microphone ("Me") and what the computer
//! plays (the other side, through WASAPI loopback), each into its own track
//! in the meeting's folder.
//!
//! Nothing here touches dictation's recorder: the microphone gets its own
//! handy-recorder `Recorder` (two recorders on one device share it in WASAPI
//! shared mode), so a dictation during a meeting works as usual.
//!
//! Audio callbacks only copy samples into a channel; one writer thread does
//! all the file work. Both tracks are kept on the meeting's wall clock: a side
//! that delivers nothing for a while (loopback is silent while nothing plays,
//! a microphone that's reconnecting) is padded with silence up to the elapsed
//! time, so the transcript can put the two sides in order by time.

use super::store::{Meeting, MeetingStore};
use super::wav::{repair_wav, TrackWriter, TRACK_SAMPLE_RATE};
use crate::managers::audio::AudioRecordingManager;
use crate::managers::transcription_jobs::TranscriptionJobManager;
use crate::settings::{get_settings, MeetingSources};
use anyhow::{anyhow, bail, Result};
use handy_recorder::{AudioChunk, Channels, ErrorKind, Recorder, RecorderConfig, Sink};
use log::{debug, error, info, warn};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Indexing, Resampler};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use tauri_specta::Event;

/// The microphone's track in a meeting's folder.
pub const MICROPHONE_TRACK: &str = "microphone.wav";
/// The system audio's track in a meeting's folder.
pub const SYSTEM_TRACK: &str = "system.wav";
/// Present while the meeting records; a launch that finds its row still
/// recording recovers the tracks.
pub const RECORDING_LOCK: &str = "recording.json";

/// A side that has delivered nothing for this long is behind the wall clock
/// and gets padded. Longer than any ordinary gap between deliveries (30 ms
/// for the microphone, ~10 ms for loopback), so silence never lands in the
/// middle of flowing audio.
const PAD_AFTER: Duration = Duration::from_millis(300);
/// Padding stops this far short of the wall clock: audio that resumes was
/// captured a little before it arrives.
const PAD_MARGIN_MS: u64 = 100;
/// How often the writer checks for sides to pad.
const PAD_CHECK: Duration = Duration::from_millis(200);
/// How often the tracks' headers are rewritten, bounding what a crash loses.
const CHECKPOINT: Duration = Duration::from_secs(1);
/// The supervisor's tick: level events and source failure checks.
const SUPERVISE_EVERY: Duration = Duration::from_millis(100);
/// Between attempts to reopen a lost source.
const RECONNECT_EVERY: Duration = Duration::from_secs(2);
/// App exit waits at most this long for the tracks to be saved.
const SHUTDOWN_LIMIT: Duration = Duration::from_secs(3);
/// Loopback input frames per resampler step (~21 ms at 48 kHz).
const RESAMPLER_CHUNK_FRAMES: usize = 1024;
/// Quietest level the meter shows, in dBFS.
const LEVEL_FLOOR_DB: f32 = -60.0;

/// What one side of the meeting is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingSourceState {
    /// Not recorded: the setting leaves it out.
    Off,
    Recording,
    /// Lost its device and is opening it again; the gap is kept as silence.
    Reconnecting,
    /// Couldn't be opened; the meeting records without it.
    Failed,
}

/// The meeting being recorded. `started_at` is in Unix seconds.
#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct MeetingRecording {
    pub meeting_id: i64,
    pub started_at: i64,
    pub elapsed_ms: i64,
    pub microphone: MeetingSourceState,
    pub system_audio: MeetingSourceState,
}

/// A meeting started or stopped recording, or a source's state changed.
#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct MeetingRecordingChanged {
    pub recording: Option<MeetingRecording>,
}

/// Input levels of the meeting being recorded, about ten times a second.
/// Levels run 0-1.
#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct MeetingLevels {
    pub meeting_id: i64,
    pub elapsed_ms: i64,
    pub microphone: f32,
    pub system_audio: f32,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/* ──────────────────────────────────────────────────────────────── */

/// The loudest chunk's RMS since the meter last read it. Written from audio
/// callbacks, so lock-free: positive floats order like their bits.
#[derive(Default)]
struct Level(AtomicU32);

impl Level {
    fn record(&self, samples: &[f32]) {
        if samples.is_empty() {
            return;
        }
        let power = samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32;
        self.0.fetch_max(power.sqrt().to_bits(), Ordering::Relaxed);
    }

    /// The level since the last call, 0-1 on a dB scale (what the ear
    /// hears), and resets it.
    fn take(&self) -> f32 {
        perceptual_level(f32::from_bits(self.0.swap(0, Ordering::Relaxed)))
    }
}

fn perceptual_level(rms: f32) -> f32 {
    if rms <= 0.0 || !rms.is_finite() {
        return 0.0;
    }
    ((20.0 * rms.log10() - LEVEL_FLOOR_DB) / -LEVEL_FLOOR_DB).clamp(0.0, 1.0)
}

/// One opened source's own flags, so a late callback of a replaced source
/// can't mark its replacement failed.
#[derive(Default)]
struct SourceFlags {
    failed: AtomicBool,
    /// Has delivered audio.
    flowing: AtomicBool,
}

enum Msg {
    /// 16 kHz mono from the microphone.
    Microphone(Vec<f32>),
    /// Interleaved loopback audio at the output device's format.
    System {
        samples: Vec<f32>,
        rate: u32,
        channels: u16,
    },
    Stop,
}

/* ──────────────────────────── microphone ──────────────────────────── */

struct MicSink {
    tx: Sender<Msg>,
    flags: Arc<SourceFlags>,
    level: Arc<Level>,
}

impl Sink for MicSink {
    fn process_chunk(&mut self, chunk: AudioChunk<'_>) {
        let samples = &chunk.samples[..chunk.valid_frames * chunk.channels as usize];
        self.flags.flowing.store(true, Ordering::Release);
        self.level.record(samples);
        // The writer is gone only once the meeting has stopped.
        let _ = self.tx.send(Msg::Microphone(samples.to_vec()));
    }
}

/// An opened microphone, not yet recording.
struct OpenedMicrophone {
    recorder: Recorder<MicSink>,
    sink: MicSink,
    flags: Arc<SourceFlags>,
}

impl OpenedMicrophone {
    fn start(self) -> MicSource {
        if let Err(error) = self.recorder.start(self.sink) {
            // Reconnected by the supervisor like any other failure.
            warn!("Meeting microphone didn't start: {}", error.error);
            self.flags.failed.store(true, Ordering::Release);
        }
        MicSource {
            recorder: self.recorder,
            flags: self.flags,
        }
    }
}

struct MicSource {
    recorder: Recorder<MicSink>,
    flags: Arc<SourceFlags>,
}

impl MicSource {
    /// Ends the recording, so every chunk the sink got has been sent, and
    /// closes the device.
    fn close(self) {
        if let Err(error) = self.recorder.stop() {
            debug!("Meeting microphone stop: {error}");
        }
        drop(self.recorder);
    }
}

fn open_recorder(
    device: Option<String>,
    channel: Option<u16>,
    take_headset: bool,
    sink: impl FnOnce() -> MicSink,
) -> Result<OpenedMicrophone, handy_recorder::Error> {
    let config = |channels: Channels| RecorderConfig {
        device: device.clone(),
        channels,
        take_headset,
        ..RecorderConfig::speech()
    };
    let flags = Arc::new(SourceFlags::default());
    let open = |channels: Channels| {
        Recorder::<MicSink>::open_with_failure_handler(config(channels), {
            let flags = Arc::clone(&flags);
            move |error: handy_recorder::Error| {
                warn!("Meeting microphone failed: {error}");
                flags.failed.store(true, Ordering::Release);
            }
        })
    };
    let recorder = match open(channel.map_or(Channels::MixToMono, Channels::Only)) {
        // The channel was picked on another microphone, as dictation does.
        Err(error) if error.kind() == ErrorKind::InvalidChannel => {
            warn!("{error}; averaging all input channels for the meeting instead");
            open(Channels::MixToMono)?
        }
        result => result?,
    };
    let mut sink = sink();
    sink.flags = Arc::clone(&flags);
    Ok(OpenedMicrophone {
        recorder,
        sink,
        flags,
    })
}

/// Opens the dictation microphone (the selected one, or the default when it
/// isn't connected), resolved as dictation resolves it. A remembered device
/// ID that no longer opens is looked up again, and the default is the last
/// resort.
fn open_microphone(
    app: &AppHandle,
    tx: &Sender<Msg>,
    level: &Arc<Level>,
) -> Result<OpenedMicrophone> {
    let settings = get_settings(app);
    let audio = app
        .try_state::<Arc<AudioRecordingManager>>()
        .ok_or_else(|| anyhow!("The audio manager isn't ready"))?;
    let new_sink = || MicSink {
        tx: tx.clone(),
        flags: Arc::default(),
        level: Arc::clone(level),
    };

    let mut tried: Vec<Option<String>> = Vec::new();
    let mut last_error = None;
    for fresh in [false, true] {
        let device = audio.meeting_microphone_device(fresh);
        for candidate in [device, None] {
            if tried.contains(&candidate) {
                continue;
            }
            tried.push(candidate.clone());
            match open_recorder(
                candidate.clone(),
                settings.selected_channel,
                settings.airpods_mode,
                &new_sink,
            ) {
                Ok(opened) => {
                    info!(
                        "Meeting microphone opened: {}",
                        opened.recorder.info().device.name
                    );
                    return Ok(opened);
                }
                Err(error) => {
                    debug!("Meeting microphone {candidate:?} didn't open: {error}");
                    last_error = Some(error);
                }
            }
            // The resolved device failed; look it up again before the default.
            if !fresh {
                break;
            }
        }
    }
    Err(match last_error {
        Some(error) => anyhow!(error),
        None => anyhow!("No microphone to open"),
    })
}

/* ──────────────────────────── system audio ────────────────────────── */

#[cfg(windows)]
mod loopback {
    use super::{Level, Msg, SourceFlags};
    use anyhow::{anyhow, bail, Result};
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::{FromSample, SampleFormat, SizedSample};
    use log::{info, warn};
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::Sender;
    use std::sync::Arc;
    use std::time::Duration;

    const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

    /// A loopback stream on the default output device: whatever the
    /// computer plays. Built on the *default* handle, so when the default
    /// output changes the stream reports an error and is reopened on the new
    /// one. Dropping it stops the stream; no callback runs after that.
    pub(super) struct SystemSource {
        stream: cpal::Stream,
        pub(super) flags: Arc<SourceFlags>,
    }

    impl SystemSource {
        pub(super) fn play(&self) -> Result<()> {
            self.stream
                .play()
                .map_err(|e| anyhow!("System audio didn't start: {e}"))
        }
    }

    pub(super) fn open(tx: &Sender<Msg>, level: &Arc<Level>) -> Result<SystemSource> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or_else(|| anyhow!("There is no audio output device"))?;
        // An input stream on a render device is a loopback capture, at the
        // device's mix format.
        let supported = device
            .default_output_config()
            .map_err(|e| anyhow!("Couldn't read the output device's format: {e}"))?;
        let config = supported.config();
        let flags = Arc::new(SourceFlags::default());
        let (rate, channels) = (config.sample_rate, config.channels);
        let stream = match supported.sample_format() {
            SampleFormat::F32 => build::<f32>(&device, config, tx, &flags, level),
            SampleFormat::I16 => build::<i16>(&device, config, tx, &flags, level),
            SampleFormat::I32 => build::<i32>(&device, config, tx, &flags, level),
            other => bail!("The output device's sample format {other:?} isn't supported"),
        }?;
        info!("System audio opened: {rate} Hz, {channels} channels");
        Ok(SystemSource { stream, flags })
    }

    fn build<T>(
        device: &cpal::Device,
        config: cpal::StreamConfig,
        tx: &Sender<Msg>,
        flags: &Arc<SourceFlags>,
        level: &Arc<Level>,
    ) -> Result<cpal::Stream>
    where
        T: SizedSample,
        f32: FromSample<T>,
    {
        let (rate, channels) = (config.sample_rate, config.channels);
        let tx = tx.clone();
        let level = Arc::clone(level);
        let data_flags = Arc::clone(flags);
        let error_flags = Arc::clone(flags);
        device
            .build_input_stream::<T, _, _>(
                config,
                // The real-time audio thread: copy and hand off, nothing more.
                move |data: &[T], _: &_| {
                    let samples: Vec<f32> = data
                        .iter()
                        .map(|&s| <f32 as FromSample<T>>::from_sample_(s))
                        .collect();
                    data_flags.flowing.store(true, Ordering::Release);
                    level.record(&samples);
                    let _ = tx.send(Msg::System {
                        samples,
                        rate,
                        channels,
                    });
                },
                move |error: cpal::Error| {
                    warn!("System audio failed: {error}");
                    error_flags.failed.store(true, Ordering::Release);
                },
                Some(OPEN_TIMEOUT),
            )
            .map_err(|e| anyhow!("Couldn't open system audio: {e}"))
    }
}

#[cfg(not(windows))]
mod loopback {
    use super::{Level, Msg, SourceFlags};
    use anyhow::{bail, Result};
    use std::sync::mpsc::Sender;
    use std::sync::Arc;

    /// System audio is captured through WASAPI loopback, which only Windows
    /// has; elsewhere the side can't open.
    pub(super) enum SystemSource {}

    impl SystemSource {
        pub(super) fn play(&self) -> Result<()> {
            match *self {}
        }
    }

    impl SystemSource {
        pub(super) fn flags(&self) -> &Arc<SourceFlags> {
            match *self {}
        }
    }

    pub(super) fn open(_tx: &Sender<Msg>, _level: &Arc<Level>) -> Result<SystemSource> {
        bail!("Recording system audio needs Windows")
    }
}

use loopback::SystemSource;

#[cfg(windows)]
fn system_flags(source: &SystemSource) -> &Arc<SourceFlags> {
    &source.flags
}

#[cfg(not(windows))]
fn system_flags(source: &SystemSource) -> &Arc<SourceFlags> {
    source.flags()
}

/* ──────────────────────────── resampling ──────────────────────────── */

/// Loopback audio (mono, at the output device's rate) to 16 kHz, a step at
/// a time.
struct StreamResampler {
    rate: u32,
    /// `None` when the device already runs at 16 kHz.
    stage: Option<Stage>,
}

struct Stage {
    resampler: Fft<f32>,
    /// Input not yet resampled: less than one step.
    pending: Vec<f32>,
    /// One step's output.
    scratch: Vec<f32>,
    /// Output frames of the resampler's delay still to drop, so the track
    /// isn't shifted late by it.
    delay_left: usize,
}

impl StreamResampler {
    fn new(rate: u32) -> Result<Self> {
        let stage = if rate == TRACK_SAMPLE_RATE {
            None
        } else {
            let resampler = Fft::<f32>::new(
                rate as usize,
                TRACK_SAMPLE_RATE as usize,
                RESAMPLER_CHUNK_FRAMES,
                1,
                FixedSync::Input,
            )
            .map_err(|e| anyhow!("Failed to set up resampling from {rate} Hz: {e}"))?;
            Some(Stage {
                pending: Vec::with_capacity(2 * resampler.input_frames_max()),
                scratch: vec![0.0; resampler.output_frames_max()],
                delay_left: resampler.output_delay(),
                resampler,
            })
        };
        Ok(Self { rate, stage })
    }

    fn push(&mut self, mono: impl Iterator<Item = f32>, out: &mut Vec<f32>) -> Result<()> {
        let Some(stage) = self.stage.as_mut() else {
            out.extend(mono);
            return Ok(());
        };
        stage.pending.extend(mono);
        let mut taken = 0;
        while stage.pending.len() - taken >= stage.resampler.input_frames_next() {
            taken += stage.step(taken, None, out)?;
        }
        stage.pending.drain(..taken);
        Ok(())
    }

    /// Resamples what's pending, short of a full step: before a gap is padded
    /// (so the audio comes before the silence), on a format change, and at
    /// the end.
    fn flush(&mut self, out: &mut Vec<f32>) -> Result<()> {
        let Some(stage) = self.stage.as_mut() else {
            return Ok(());
        };
        let left = stage.pending.len();
        if left == 0 {
            return Ok(());
        }
        let before = out.len();
        stage.step(0, Some(left), out)?;
        stage.pending.clear();
        // The step's output covers a whole step; keep only the real part.
        let real = (stage.resampler.resample_ratio() * left as f64).ceil() as usize;
        out.truncate((before + real).min(out.len()));
        Ok(())
    }
}

impl Stage {
    fn step(&mut self, offset: usize, partial: Option<usize>, out: &mut Vec<f32>) -> Result<usize> {
        let input = InterleavedSlice::new(&self.pending[..], 1, self.pending.len())
            .map_err(|e| anyhow!("Failed to resample: {e}"))?;
        let scratch_len = self.scratch.len();
        let mut output = InterleavedSlice::new_mut(&mut self.scratch[..], 1, scratch_len)
            .map_err(|e| anyhow!("Failed to resample: {e}"))?;
        let mut indexing = Indexing::new().input_offset(offset);
        indexing.partial_len = partial;
        let (taken, produced) = self
            .resampler
            .process_into_buffer(&input, &mut output, Some(&indexing))
            .map_err(|e| anyhow!("Failed to resample: {e}"))?;
        let dropped = self.delay_left.min(produced);
        self.delay_left -= dropped;
        out.extend_from_slice(&self.scratch[dropped..produced]);
        Ok(taken)
    }
}

/* ──────────────────────────── writer thread ───────────────────────── */

struct Track {
    name: &'static str,
    /// `None` after a write error: the track keeps what it had.
    writer: Option<TrackWriter>,
    samples: u64,
    last_audio: Instant,
}

impl Track {
    fn new(name: &'static str, writer: TrackWriter, start: Instant) -> Self {
        Self {
            name,
            writer: Some(writer),
            samples: 0,
            last_audio: start,
        }
    }

    fn write(&mut self, op: impl FnOnce(&mut TrackWriter) -> Result<()>) {
        let Some(writer) = self.writer.as_mut() else {
            return;
        };
        if let Err(error) = op(writer) {
            error!("Meeting track {} stopped recording: {error:#}", self.name);
            // Finalizing keeps what was written readable.
            if let Some(writer) = self.writer.take() {
                let _ = writer.finish();
            }
            return;
        }
        self.samples = writer.samples();
    }

    fn finish(self) -> u64 {
        if let Some(writer) = self.writer {
            if let Err(error) = writer.finish() {
                error!("Failed to finish meeting track {}: {error:#}", self.name);
            }
        }
        self.samples
    }
}

/// Samples each side's track got; 0 for a side without a track.
struct WriterResult {
    microphone: u64,
    system: u64,
}

struct Writer {
    start: Instant,
    microphone: Option<Track>,
    system: Option<Track>,
    resampler: Option<StreamResampler>,
    /// Reused resampler output.
    out: Vec<f32>,
}

impl Writer {
    fn run(mut self, rx: Receiver<Msg>) -> WriterResult {
        let mut last_pad = Instant::now();
        let mut last_checkpoint = Instant::now();
        loop {
            match rx.recv_timeout(PAD_CHECK) {
                Ok(Msg::Microphone(samples)) => {
                    if let Some(track) = self.microphone.as_mut() {
                        track.last_audio = Instant::now();
                        track.write(|w| w.push(&samples));
                    }
                }
                Ok(Msg::System {
                    samples,
                    rate,
                    channels,
                }) => self.system_audio(&samples, rate, channels),
                Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
            if last_pad.elapsed() >= PAD_CHECK {
                self.pad();
                last_pad = Instant::now();
            }
            if last_checkpoint.elapsed() >= CHECKPOINT {
                for track in [self.microphone.as_mut(), self.system.as_mut()]
                    .into_iter()
                    .flatten()
                {
                    track.write(|w| w.flush());
                }
                last_checkpoint = Instant::now();
            }
        }
        self.flush_resampler();
        WriterResult {
            microphone: self.microphone.map_or(0, Track::finish),
            system: self.system.map_or(0, Track::finish),
        }
    }

    fn system_audio(&mut self, samples: &[f32], rate: u32, channels: u16) {
        let Some(track) = self.system.as_mut() else {
            return;
        };
        track.last_audio = Instant::now();
        // A reopened stream can come back at another rate.
        if self.resampler.as_ref().is_none_or(|r| r.rate != rate) {
            self.flush_resampler();
            match StreamResampler::new(rate) {
                Ok(resampler) => self.resampler = Some(resampler),
                Err(error) => {
                    error!("{error:#}");
                    self.resampler = None;
                    return;
                }
            }
        }
        let (Some(track), Some(resampler)) = (self.system.as_mut(), self.resampler.as_mut()) else {
            return;
        };
        let channels = channels.max(1) as usize;
        let mono = samples
            .chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32);
        self.out.clear();
        if let Err(error) = resampler.push(mono, &mut self.out) {
            error!("System audio: {error:#}");
        }
        let out = &self.out;
        track.write(|w| w.push(out));
    }

    fn flush_resampler(&mut self) {
        let (Some(track), Some(resampler)) = (self.system.as_mut(), self.resampler.as_mut()) else {
            return;
        };
        self.out.clear();
        if let Err(error) = resampler.flush(&mut self.out) {
            error!("System audio: {error:#}");
        }
        if !self.out.is_empty() {
            let out = &self.out;
            track.write(|w| w.push(out));
        }
    }

    /// Fills a side that has gone quiet with silence up to the wall clock
    /// (less a margin). Audio is never dropped to catch up: a side ahead of
    /// the clock stays ahead.
    fn pad(&mut self) {
        let elapsed_ms = self.start.elapsed().as_millis() as u64;
        let target = elapsed_ms.saturating_sub(PAD_MARGIN_MS) * TRACK_SAMPLE_RATE as u64 / 1000;
        let behind =
            |track: &Track| track.last_audio.elapsed() >= PAD_AFTER && track.samples < target;

        if self.microphone.as_ref().is_some_and(behind) {
            let track = self.microphone.as_mut().expect("checked");
            let missing = target - track.samples;
            track.write(|w| w.pad_silence(missing));
        }
        if self.system.as_ref().is_some_and(behind) {
            // What's still in the resampler came before the gap.
            self.flush_resampler();
            let track = self.system.as_mut().expect("checked");
            let missing = target.saturating_sub(track.samples);
            track.write(|w| w.pad_silence(missing));
        }
    }
}

/* ──────────────────────────── session ─────────────────────────────── */

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Microphone,
    System,
}

struct SideState {
    state: Mutex<MeetingSourceState>,
    level: Arc<Level>,
    /// A reconnect thread is running for this side.
    reconnecting: AtomicBool,
}

impl SideState {
    fn new(state: MeetingSourceState, level: Arc<Level>) -> Self {
        Self {
            state: Mutex::new(state),
            level,
            reconnecting: AtomicBool::new(false),
        }
    }

    fn get(&self) -> MeetingSourceState {
        *lock(&self.state)
    }
}

#[derive(Default)]
struct Sources {
    microphone: Option<MicSource>,
    system: Option<SystemSource>,
    /// The meeting has stopped: a reconnect that finishes now closes what it
    /// opened instead of installing it.
    stopped: bool,
}

/// One meeting being recorded.
struct Session {
    app: AppHandle,
    meeting_id: i64,
    started_at: i64,
    start: Instant,
    folder: PathBuf,
    tx: Sender<Msg>,
    microphone: SideState,
    system: SideState,
    sources: Mutex<Sources>,
    stopping: AtomicBool,
    writer: Mutex<Option<JoinHandle<WriterResult>>>,
}

impl Session {
    fn side(&self, side: Side) -> &SideState {
        match side {
            Side::Microphone => &self.microphone,
            Side::System => &self.system,
        }
    }

    fn elapsed_ms(&self) -> i64 {
        self.start.elapsed().as_millis() as i64
    }

    fn snapshot(&self) -> MeetingRecording {
        MeetingRecording {
            meeting_id: self.meeting_id,
            started_at: self.started_at,
            elapsed_ms: self.elapsed_ms(),
            microphone: self.microphone.get(),
            system_audio: self.system.get(),
        }
    }

    fn set_state(&self, side: Side, state: MeetingSourceState) {
        let changed = {
            let mut current = lock(&self.side(side).state);
            std::mem::replace(&mut *current, state) != state
        };
        if changed {
            info!("Meeting {:?}: {:?}", side, state);
            emit_recording_changed(&self.app, Some(self.snapshot()));
        }
    }

    fn stopping(&self) -> bool {
        self.stopping.load(Ordering::Acquire)
    }

    /// Watches the sources and reports levels until the meeting stops.
    fn supervise(self: Arc<Self>) {
        while !self.stopping() {
            thread::sleep(SUPERVISE_EVERY);
            if self.stopping() {
                break;
            }

            let (dead_microphone, dead_system, microphone_flowing) = {
                let mut sources = lock(&self.sources);
                let dead_microphone = if sources
                    .microphone
                    .as_ref()
                    .is_some_and(|m| m.flags.failed.load(Ordering::Acquire))
                {
                    sources.microphone.take()
                } else {
                    None
                };
                let dead_system = if sources
                    .system
                    .as_ref()
                    .is_some_and(|s| system_flags(s).failed.load(Ordering::Acquire))
                {
                    sources.system.take()
                } else {
                    None
                };
                let microphone_flowing = sources
                    .microphone
                    .as_ref()
                    .is_some_and(|m| m.flags.flowing.load(Ordering::Acquire));
                (dead_microphone, dead_system, microphone_flowing)
            };

            if let Some(dead) = dead_microphone {
                self.set_state(Side::Microphone, MeetingSourceState::Reconnecting);
                self.spawn_reconnect(Side::Microphone, move |session| {
                    session.reconnect_microphone(dead)
                });
            } else if microphone_flowing
                && self.microphone.get() == MeetingSourceState::Reconnecting
            {
                self.set_state(Side::Microphone, MeetingSourceState::Recording);
            }
            if let Some(dead) = dead_system {
                self.set_state(Side::System, MeetingSourceState::Reconnecting);
                self.spawn_reconnect(Side::System, move |session| session.reconnect_system(dead));
            }

            let level = |side: &SideState| {
                let level = side.level.take();
                match side.get() {
                    MeetingSourceState::Off | MeetingSourceState::Failed => 0.0,
                    _ => level,
                }
            };
            let levels = MeetingLevels {
                meeting_id: self.meeting_id,
                elapsed_ms: self.elapsed_ms(),
                microphone: level(&self.microphone),
                system_audio: level(&self.system),
            };
            if let Err(error) = levels.emit(&self.app) {
                debug!("Failed to emit meeting levels: {error}");
            }
        }
    }

    fn spawn_reconnect(
        self: &Arc<Self>,
        side: Side,
        reconnect: impl FnOnce(&Arc<Session>) + Send + 'static,
    ) {
        if self.side(side).reconnecting.swap(true, Ordering::AcqRel) {
            return;
        }
        let session = Arc::clone(self);
        let spawned = thread::Builder::new()
            .name(format!("meeting-reconnect-{side:?}").to_lowercase())
            .spawn(move || {
                reconnect(&session);
                session
                    .side(side)
                    .reconnecting
                    .store(false, Ordering::Release);
            });
        if let Err(error) = spawned {
            error!("Couldn't start reconnecting the meeting's {side:?}: {error}");
            self.side(side).reconnecting.store(false, Ordering::Release);
        }
    }

    /// Sleeps between attempts, waking early when the meeting stops.
    fn wait_to_retry(&self) -> bool {
        let until = Instant::now() + RECONNECT_EVERY;
        while Instant::now() < until {
            if self.stopping() {
                return false;
            }
            thread::sleep(SUPERVISE_EVERY);
        }
        !self.stopping()
    }

    /// Reopens the microphone (the same device by name, else the default)
    /// until it opens or the meeting stops. It's back to `Recording` once
    /// audio flows; padding keeps the gap as silence.
    fn reconnect_microphone(&self, dead: MicSource) {
        dead.close();
        loop {
            if self.stopping() {
                return;
            }
            match open_microphone(&self.app, &self.tx, &self.microphone.level) {
                Ok(opened) => {
                    let mut sources = lock(&self.sources);
                    if sources.stopped {
                        drop(sources);
                        drop(opened);
                        return;
                    }
                    sources.microphone = Some(opened.start());
                    info!("Meeting microphone reconnected");
                    return;
                }
                Err(error) => debug!("Meeting microphone still unavailable: {error:#}"),
            }
            if !self.wait_to_retry() {
                return;
            }
        }
    }

    /// Reopens loopback on the (possibly new) default output device.
    /// Loopback delivers nothing while nothing plays, so it counts as
    /// recording again as soon as the stream runs.
    fn reconnect_system(&self, dead: SystemSource) {
        drop(dead);
        loop {
            if self.stopping() {
                return;
            }
            match loopback::open(&self.tx, &self.system.level).and_then(|source| {
                source.play()?;
                Ok(source)
            }) {
                Ok(source) => {
                    let mut sources = lock(&self.sources);
                    if sources.stopped {
                        drop(sources);
                        drop(source);
                        return;
                    }
                    sources.system = Some(source);
                    drop(sources);
                    self.set_state(Side::System, MeetingSourceState::Recording);
                    return;
                }
                Err(error) => debug!("System audio still unavailable: {error:#}"),
            }
            if !self.wait_to_retry() {
                return;
            }
        }
    }

    /// Stops the sources, saves the tracks and the meeting's row. Queues its
    /// transcription when `enqueue` (not at app exit: the queued row is
    /// picked up on the next launch).
    fn finish(&self, enqueue: bool) -> Result<Meeting> {
        let duration_ms = self.elapsed_ms();
        self.stopping.store(true, Ordering::Release);
        let (microphone, system) = {
            let mut sources = lock(&self.sources);
            sources.stopped = true;
            (sources.microphone.take(), sources.system.take())
        };
        if let Some(microphone) = microphone {
            microphone.close();
        }
        drop(system);
        // Everything the sources delivered is queued ahead of this.
        let _ = self.tx.send(Msg::Stop);

        let microphone_path = self.folder.join(MICROPHONE_TRACK);
        let system_path = self.folder.join(SYSTEM_TRACK);
        let written = match lock(&self.writer).take().map(JoinHandle::join) {
            Some(Ok(result)) => result,
            other => {
                if matches!(other, Some(Err(_))) {
                    error!("The meeting's writer thread panicked; recovering its tracks");
                }
                WriterResult {
                    microphone: recover_track(&microphone_path),
                    system: recover_track(&system_path),
                }
            }
        };
        for (path, samples) in [
            (&microphone_path, written.microphone),
            (&system_path, written.system),
        ] {
            if samples == 0 {
                remove_if_exists(path);
            }
        }

        let store = self
            .app
            .try_state::<Arc<MeetingStore>>()
            .ok_or_else(|| anyhow!("Meeting storage isn't ready"))?;
        if written.microphone == 0 && written.system == 0 {
            info!(
                "Meeting {} recorded no audio; discarding it",
                self.meeting_id
            );
            store.discard_recording(self.meeting_id)?;
            bail!("Nothing was recorded");
        }
        let meeting = store.finish_recording(
            self.meeting_id,
            duration_ms,
            written.microphone > 0,
            written.system > 0,
            false,
        )?;
        remove_if_exists(&self.folder.join(RECORDING_LOCK));
        info!(
            "Meeting {} saved: {} ms (microphone {} samples, system audio {} samples)",
            self.meeting_id, duration_ms, written.microphone, written.system
        );
        if enqueue {
            enqueue_transcription(&self.app, self.meeting_id);
        }
        Ok(meeting)
    }
}

fn remove_if_exists(path: &Path) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => warn!("Failed to remove {}: {error}", path.display()),
    }
}

fn enqueue_transcription(app: &AppHandle, meeting_id: i64) {
    match app.try_state::<Arc<TranscriptionJobManager>>() {
        Some(jobs) => {
            if let Err(error) = jobs.enqueue_meeting(meeting_id, None) {
                error!("Failed to queue meeting {meeting_id} for transcription: {error:#}");
            }
        }
        None => error!("Can't queue meeting {meeting_id}: transcription jobs aren't ready"),
    }
}

fn emit_recording_changed(app: &AppHandle, recording: Option<MeetingRecording>) {
    if let Err(error) = (MeetingRecordingChanged { recording }).emit(app) {
        warn!("Failed to emit meeting-recording-changed: {error}");
    }
}

/// Samples in a track left by an unfinished recording, after fixing its
/// header; 0 when it's missing or unreadable.
fn recover_track(path: &Path) -> u64 {
    if !path.exists() {
        return 0;
    }
    match repair_wav(path) {
        Ok(samples) => samples,
        Err(error) => {
            warn!("Couldn't repair {}: {error:#}", path.display());
            0
        }
    }
}

/* ──────────────────────────── recorder ────────────────────────────── */

/// Records meetings, one at a time. Managed as `Arc<MeetingRecorder>`.
pub struct MeetingRecorder {
    app: AppHandle,
    /// Serializes start and stop, which open and close devices (slow).
    busy: Mutex<()>,
    /// The meeting being recorded. Held only briefly, so status reads never
    /// wait on a device.
    active: Mutex<Option<Arc<Session>>>,
}

impl MeetingRecorder {
    pub fn new(app: &AppHandle) -> Arc<Self> {
        Arc::new(Self {
            app: app.clone(),
            busy: Mutex::new(()),
            active: Mutex::new(None),
        })
    }

    /// Starts recording from the sources `meeting_sources` asks for. Fails
    /// when a meeting is already recording or no source could open; then
    /// nothing is left behind.
    pub fn start(&self) -> Result<Meeting> {
        let _busy = lock(&self.busy);
        if lock(&self.active).is_some() {
            bail!("A meeting is already recording");
        }
        let store = self
            .app
            .try_state::<Arc<MeetingStore>>()
            .ok_or_else(|| anyhow!("Meeting storage isn't ready"))?;

        let settings = get_settings(&self.app);
        let (want_microphone, want_system) = match settings.meeting_sources {
            MeetingSources::Both => (true, true),
            MeetingSources::Microphone => (true, false),
            MeetingSources::System => (false, true),
        };

        let (tx, rx) = mpsc::channel();
        let microphone_level = Arc::new(Level::default());
        let system_level = Arc::new(Level::default());
        let mut errors = Vec::new();
        let microphone = if want_microphone {
            open_microphone(&self.app, &tx, &microphone_level)
                .inspect_err(|error| {
                    warn!("Meeting microphone couldn't open: {error:#}");
                    errors.push(format!("microphone: {error:#}"));
                })
                .ok()
        } else {
            None
        };
        let system = if want_system {
            loopback::open(&tx, &system_level)
                .inspect_err(|error| {
                    warn!("Meeting system audio couldn't open: {error:#}");
                    errors.push(format!("system audio: {error:#}"));
                })
                .ok()
        } else {
            None
        };
        if microphone.is_none() && system.is_none() {
            bail!("No audio source could open ({})", errors.join("; "));
        }

        let started_at = chrono::Utc::now().timestamp();
        let meeting = store.create_recording(started_at, microphone.is_some(), system.is_some())?;
        let folder = store.folder(meeting.id);
        let start = Instant::now();
        let prepared = (|| -> Result<JoinHandle<WriterResult>> {
            fs::write(
                folder.join(RECORDING_LOCK),
                serde_json::to_vec(&serde_json::json!({
                    "id": meeting.id,
                    "started_at": started_at,
                }))?,
            )?;
            let track = |present: bool, name: &'static str| -> Result<Option<Track>> {
                Ok(if present {
                    Some(Track::new(
                        name,
                        TrackWriter::create(&folder.join(name))?,
                        start,
                    ))
                } else {
                    None
                })
            };
            let writer = Writer {
                start,
                microphone: track(microphone.is_some(), MICROPHONE_TRACK)?,
                system: track(system.is_some(), SYSTEM_TRACK)?,
                resampler: None,
                out: Vec::new(),
            };
            Ok(thread::Builder::new()
                .name("meeting-writer".into())
                .spawn(move || writer.run(rx))?)
        })();
        let writer = match prepared {
            Ok(writer) => writer,
            Err(error) => {
                drop((microphone, system));
                if let Err(discard) = store.discard_recording(meeting.id) {
                    warn!("Failed to discard meeting {}: {discard:#}", meeting.id);
                }
                return Err(error);
            }
        };

        let state = |wanted: bool, opened: bool| match (wanted, opened) {
            (false, _) => MeetingSourceState::Off,
            (true, true) => MeetingSourceState::Recording,
            (true, false) => MeetingSourceState::Failed,
        };
        let session = Arc::new(Session {
            app: self.app.clone(),
            meeting_id: meeting.id,
            started_at,
            start,
            folder,
            tx,
            microphone: SideState::new(
                state(want_microphone, microphone.is_some()),
                microphone_level,
            ),
            system: SideState::new(state(want_system, system.is_some()), system_level),
            sources: Mutex::new(Sources::default()),
            stopping: AtomicBool::new(false),
            writer: Mutex::new(Some(writer)),
        });
        {
            let mut sources = lock(&session.sources);
            sources.microphone = microphone.map(OpenedMicrophone::start);
            if let Some(system) = system {
                if let Err(error) = system.play() {
                    // Reopened by the supervisor.
                    warn!("{error:#}");
                    system_flags(&system).failed.store(true, Ordering::Release);
                }
                sources.system = Some(system);
            }
        }
        *lock(&self.active) = Some(Arc::clone(&session));

        let supervisor = Arc::clone(&session);
        if let Err(error) = thread::Builder::new()
            .name("meeting-supervisor".into())
            .spawn(move || supervisor.supervise())
        {
            // Recording still works; only reconnects and levels are missing.
            error!("Couldn't start the meeting supervisor: {error}");
        }

        // Dictation's mute-while-recording would silence the other side.
        if let Some(audio) = self.app.try_state::<Arc<AudioRecordingManager>>() {
            audio.remove_mute();
        }
        crate::tray::set_meeting_recording(&self.app, true);
        emit_recording_changed(&self.app, Some(session.snapshot()));
        info!(
            "Meeting {} recording (microphone {:?}, system audio {:?})",
            meeting.id,
            session.microphone.get(),
            session.system.get()
        );
        Ok(meeting)
    }

    /// Stops the meeting being recorded, saves its audio and queues its
    /// transcription.
    pub fn stop(&self) -> Result<Meeting> {
        let _busy = lock(&self.busy);
        let session = self.take_active()?;
        session.finish(true)
    }

    pub fn toggle(&self) -> Result<Meeting> {
        if self.is_recording() {
            self.stop()
        } else {
            self.start()
        }
    }

    pub fn recording(&self) -> Option<MeetingRecording> {
        lock(&self.active)
            .as_ref()
            .map(|session| session.snapshot())
    }

    pub fn is_recording(&self) -> bool {
        lock(&self.active).is_some()
    }

    /// App exit: finalize tracks and finish_recording (status queued; its job
    /// resumes next launch). Never blocks > 3 s: past that the tracks are
    /// left as checkpointed, and the next launch recovers them.
    pub fn shutdown(&self) {
        let Ok(session) = self.take_active() else {
            return;
        };
        info!("Saving meeting {} before exit", session.meeting_id);
        let (done_tx, done_rx) = mpsc::channel();
        let spawned = thread::Builder::new()
            .name("meeting-shutdown".into())
            .spawn(move || {
                let _ = done_tx.send(session.finish(false));
            });
        if let Err(error) = spawned {
            error!("Couldn't save the meeting before exit: {error}");
            return;
        }
        match done_rx.recv_timeout(SHUTDOWN_LIMIT) {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => warn!("Meeting not saved at exit: {error:#}"),
            Err(_) => warn!("Saving the meeting took too long; it is recovered on the next launch"),
        }
    }

    /// Takes the active meeting out, so it reads as stopped right away.
    fn take_active(&self) -> Result<Arc<Session>> {
        let session = lock(&self.active)
            .take()
            .ok_or_else(|| anyhow!("No meeting is recording"))?;
        crate::tray::set_meeting_recording(&self.app, false);
        emit_recording_changed(&self.app, None);
        Ok(session)
    }
}

/// Launch: repair tracks of interrupted recordings, finish_recording
/// (recovered = true), enqueue. A recording that left no audio is
/// discarded.
pub fn recover_interrupted(app: &AppHandle) {
    let Some(store) = app.try_state::<Arc<MeetingStore>>() else {
        error!("Can't recover interrupted meetings: meeting storage isn't ready");
        return;
    };
    let active = app
        .try_state::<Arc<MeetingRecorder>>()
        .and_then(|recorder| recorder.recording())
        .map(|recording| recording.meeting_id);
    let interrupted = match store.interrupted_recordings() {
        Ok(meetings) => meetings,
        Err(error) => {
            error!("Failed to list interrupted meetings: {error:#}");
            return;
        }
    };
    for meeting in interrupted {
        if Some(meeting.id) == active {
            continue;
        }
        let folder = store.folder(meeting.id);
        let microphone_path = folder.join(MICROPHONE_TRACK);
        let system_path = folder.join(SYSTEM_TRACK);
        let microphone = recover_track(&microphone_path);
        let system = recover_track(&system_path);
        remove_if_exists(&folder.join(RECORDING_LOCK));

        if microphone == 0 && system == 0 {
            info!(
                "Interrupted meeting {} has no audio; discarding it",
                meeting.id
            );
            if let Err(error) = store.discard_recording(meeting.id) {
                error!("Failed to discard meeting {}: {error:#}", meeting.id);
            }
            continue;
        }
        for (path, samples) in [(&microphone_path, microphone), (&system_path, system)] {
            // An empty track that repaired fine; a corrupt one stays for the user.
            if samples == 0 && repair_wav(path).is_ok_and(|n| n == 0) {
                remove_if_exists(path);
            }
        }
        let duration_ms = (microphone.max(system) * 1000 / TRACK_SAMPLE_RATE as u64) as i64;
        match store.finish_recording(meeting.id, duration_ms, microphone > 0, system > 0, true) {
            Ok(_) => {
                info!(
                    "Recovered interrupted meeting {} ({duration_ms} ms)",
                    meeting.id
                );
                enqueue_transcription(app, meeting.id);
            }
            Err(error) => error!("Failed to recover meeting {}: {error:#}", meeting.id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_map_decibels_onto_zero_to_one() {
        assert_eq!(perceptual_level(0.0), 0.0);
        assert_eq!(perceptual_level(1.0), 1.0);
        assert_eq!(perceptual_level(0.0001), 0.0);
        assert!((perceptual_level(0.031_622_8) - 0.5).abs() < 1e-3);
    }

    #[test]
    fn level_keeps_the_loudest_chunk_until_read() {
        let level = Level::default();
        level.record(&[0.5; 10]);
        level.record(&[0.1; 10]);
        assert!((level.take() - perceptual_level(0.5)).abs() < 1e-6);
        assert_eq!(level.take(), 0.0);
    }

    #[test]
    fn resampling_48k_to_16k_keeps_the_duration() {
        let mut resampler = StreamResampler::new(48_000).unwrap();
        let mut out = Vec::new();
        for _ in 0..50 {
            resampler
                .push(std::iter::repeat_n(0.25, 480), &mut out)
                .unwrap();
        }
        resampler.flush(&mut out).unwrap();
        // 0.5 s of input, less the resampler's delay still inside it.
        let expected = 8_000usize;
        assert!(out.len() <= expected, "{} samples", out.len());
        assert!(out.len() + 400 >= expected, "{} samples", out.len());
    }

    #[test]
    fn padding_fills_a_quiet_side_up_to_the_wall_clock() {
        let dir = tempfile::tempdir().unwrap();
        let start = Instant::now() - Duration::from_secs(2);
        let path = dir.path().join(MICROPHONE_TRACK);
        let mut writer = Writer {
            start,
            microphone: Some(Track::new(
                MICROPHONE_TRACK,
                TrackWriter::create(&path).unwrap(),
                start,
            )),
            system: None,
            resampler: None,
            out: Vec::new(),
        };
        writer.pad();
        let padded = writer.microphone.as_ref().unwrap().samples;
        // About 1.9 s: two seconds, less the margin.
        assert!((30_000..=31_000).contains(&padded), "{padded}");

        // A side that just delivered isn't padded.
        let track = writer.microphone.as_mut().unwrap();
        track.last_audio = Instant::now();
        writer.start = Instant::now() - Duration::from_secs(10);
        writer.pad();
        assert_eq!(writer.microphone.as_ref().unwrap().samples, padded);
    }
}
