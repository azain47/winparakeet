//! Who spoke when on one of a meeting's tracks: Sortformer (up to four
//! speakers per track) at its offline operating point, on the CPU in this
//! process, the way VAD and language detection run.

use anyhow::{anyhow, Result};
use log::info;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use transcribe_cpp::{
    Backend, CancelToken, DiarizeExtension, DiarizeOptions, DiarizeSessionOptions, Model,
    ModelOptions, SortformerDiarizeOptions, SortformerPreset,
};

use super::transcribe::Turn;
use super::wav::read_track;
use crate::audio_toolkit::cpu_engine::init_cpu_backends;

/// How often a running diarization checks whether its job was cancelled.
const CANCEL_POLL: Duration = Duration::from_millis(200);

/// The speaker turns of `track` (16 kHz mono), by start, with the diarizer
/// at `model_path`. The whole track is diarized at once: Sortformer numbers
/// speakers across the clip it is given. `None` if `cancelled` was set while
/// it ran.
pub fn diarize(
    model_path: &Path,
    track: &Path,
    cancelled: &AtomicBool,
) -> Result<Option<Vec<Turn>>> {
    let started = Instant::now();
    let pcm = read_track(track)?;
    if pcm.is_empty() {
        return Ok(Some(Vec::new()));
    }

    init_cpu_backends()?;
    let model = Model::load_with(
        model_path,
        &ModelOptions {
            backend: Backend::Cpu,
            device: None,
        },
    )
    .map_err(|e| anyhow!("Failed to load the speaker diarizer: {e}"))?;
    // Half the cores: dictation may need the rest meanwhile.
    let threads = thread::available_parallelism()
        .map(|n| n.get() / 2)
        .unwrap_or(2)
        .max(1);
    let mut session = model
        .diarize_session_with(&DiarizeSessionOptions {
            n_threads: threads as i32,
        })
        .map_err(|e| anyhow!("Failed to start the speaker diarizer: {e}"))?;
    let token = CancelToken::new();
    session.set_cancel_token(&token);
    let options = DiarizeOptions {
        family: Some(DiarizeExtension::Sortformer(SortformerDiarizeOptions {
            preset: Some(SortformerPreset::VeryHighLatency),
        })),
    };

    let finished = AtomicBool::new(false);
    let result = thread::scope(|scope| {
        scope.spawn(|| {
            while !finished.load(Ordering::Acquire) {
                if cancelled.load(Ordering::Acquire) {
                    token.cancel();
                    return;
                }
                thread::sleep(CANCEL_POLL);
            }
        });
        let result = session.run(&pcm, &options);
        finished.store(true, Ordering::Release);
        result
    });
    let segments = match result {
        Ok(segments) => segments,
        Err(_) if cancelled.load(Ordering::Acquire) => return Ok(None),
        Err(e) => return Err(anyhow!("Speaker diarization failed: {e}")),
    };

    let mut turns: Vec<Turn> = segments
        .into_iter()
        .filter(|s| s.speaker_id > 0 && s.t1_ms > s.t0_ms)
        .map(|s| Turn {
            start_ms: s.t0_ms,
            end_ms: s.t1_ms,
            speaker: s.speaker_id,
        })
        .collect();
    // Grouped by speaker as they come, not by time.
    turns.sort_by_key(|t| (t.start_ms, t.end_ms));
    info!(
        "Diarized {:.1}s of meeting audio into {} turns in {:.1}s",
        pcm.len() as f64 / 16_000.0,
        turns.len(),
        started.elapsed().as_secs_f64()
    );
    Ok(Some(turns))
}
