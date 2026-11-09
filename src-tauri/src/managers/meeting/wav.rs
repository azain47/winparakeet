//! A meeting's audio files: one 16 kHz mono 16-bit track per side, written
//! while the meeting records, and the mix of both for playback and export.
//!
//! Tracks are written incrementally and checkpointed (header rewritten) about
//! once a second, so a crash leaves a playable file with at most the last
//! second's sizes missing from its header; [`repair_wav`] puts them back.

use anyhow::{anyhow, bail, Context, Result};
use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Every meeting track's rate.
pub const TRACK_SAMPLE_RATE: u32 = 16_000;

fn track_spec() -> WavSpec {
    WavSpec {
        channels: 1,
        sample_rate: TRACK_SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    }
}

/// The same conversion dictation recordings use (`save_wav_file`), clamped
/// so a hot sample saturates instead of wrapping.
fn to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
}

/// One side's track, written as audio arrives.
pub struct TrackWriter {
    writer: WavWriter<BufWriter<File>>,
    samples: u64,
}

impl TrackWriter {
    pub fn create(path: &Path) -> Result<Self> {
        let writer = WavWriter::create(path, track_spec())
            .with_context(|| format!("Failed to create {}", path.display()))?;
        Ok(Self { writer, samples: 0 })
    }

    pub fn push(&mut self, samples: &[f32]) -> Result<()> {
        for &sample in samples {
            self.writer.write_sample(to_i16(sample))?;
        }
        self.samples += samples.len() as u64;
        Ok(())
    }

    /// Appends `n` samples of silence: a gap in a side's audio keeps its
    /// place on the meeting's timeline.
    pub fn pad_silence(&mut self, n: u64) -> Result<()> {
        for _ in 0..n {
            self.writer.write_sample(0i16)?;
        }
        self.samples += n;
        Ok(())
    }

    /// Samples written so far.
    pub fn samples(&self) -> u64 {
        self.samples
    }

    /// Checkpoint: rewrites the header's sizes and flushes to disk, so the
    /// file is complete up to here if the app dies.
    pub fn flush(&mut self) -> Result<()> {
        self.writer.flush()?;
        Ok(())
    }

    /// Finalizes the file. Returns the samples written.
    pub fn finish(self) -> Result<u64> {
        let samples = self.samples;
        self.writer.finalize()?;
        Ok(samples)
    }
}

/// Fixes the RIFF and `data` sizes of a WAV whose writer didn't finish (the
/// app died while recording): the data runs to the end of the file, minus a
/// partial trailing sample, which is cut off. Returns the samples (frames)
/// the file holds.
pub fn repair_wav(path: &Path) -> Result<u64> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("Failed to open {}", path.display()))?;
    let file_len = file.metadata()?.len();

    let mut riff = [0u8; 12];
    file.read_exact(&mut riff)
        .map_err(|_| anyhow!("{} is too short to be a WAV file", path.display()))?;
    if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
        bail!("{} is not a WAV file", path.display());
    }

    // Walk the chunks to `data`; the writer puts it last.
    let mut offset = 12u64;
    let mut block_align: Option<u64> = None;
    let data_size_offset = loop {
        let mut header = [0u8; 8];
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut header)
            .map_err(|_| anyhow!("{} has no audio data", path.display()))?;
        let size = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as u64;
        match &header[0..4] {
            b"data" => break offset + 4,
            b"fmt " => {
                let mut fmt = [0u8; 14];
                file.read_exact(&mut fmt)
                    .map_err(|_| anyhow!("{} has a truncated format chunk", path.display()))?;
                block_align = Some(u16::from_le_bytes([fmt[12], fmt[13]]) as u64);
            }
            _ => {}
        }
        // Chunks are padded to an even size.
        offset += 8 + size + (size & 1);
    };
    let block_align = block_align
        .filter(|&align| align > 0)
        .ok_or_else(|| anyhow!("{} has no format before its data", path.display()))?;

    let data_start = data_size_offset + 4;
    let available = file_len.saturating_sub(data_start);
    let data_len = (available / block_align * block_align).min(u32::MAX as u64 - data_start);
    let new_len = data_start + data_len;
    if new_len < file_len {
        file.set_len(new_len)?;
    }

    file.seek(SeekFrom::Start(4))?;
    file.write_all(&((new_len - 8) as u32).to_le_bytes())?;
    file.seek(SeekFrom::Start(data_size_offset))?;
    file.write_all(&(data_len as u32).to_le_bytes())?;
    file.sync_all()?;
    Ok(data_len / block_align)
}

/// Opens a meeting track, refusing any other format.
fn open_track(path: &Path) -> Result<WavReader<BufReader<File>>> {
    let reader =
        WavReader::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let spec = reader.spec();
    if spec != track_spec() {
        bail!(
            "{} is not a 16 kHz mono 16-bit track ({} Hz, {} channels, {}-bit)",
            path.display(),
            spec.sample_rate,
            spec.channels,
            spec.bits_per_sample
        );
    }
    Ok(reader)
}

/// A whole track as samples, scaled like dictation recordings
/// (`read_wav_samples`).
pub fn read_track(path: &Path) -> Result<Vec<f32>> {
    open_track(path)?
        .into_samples::<i16>()
        .map(|s| s.map(|v| v as f32 / i16::MAX as f32))
        .collect::<Result<Vec<f32>, _>>()
        .with_context(|| format!("Failed to read {}", path.display()))
}

/// Mixes tracks into `out`: the samples are summed and clamped, and a shorter
/// track counts as silence after its end. Streams sample by sample, so an
/// hours-long meeting is never held in memory, and writes a temporary file
/// first, so `out` is either the old file or the complete new one.
pub fn mix_tracks(inputs: &[PathBuf], out: &Path) -> Result<()> {
    if inputs.is_empty() {
        bail!("No tracks to mix");
    }
    let mut readers = inputs
        .iter()
        .map(|path| Ok(open_track(path)?.into_samples::<i16>()))
        .collect::<Result<Vec<_>>>()?;

    let mut tmp_name = out
        .file_name()
        .ok_or_else(|| anyhow!("{} is not a file path", out.display()))?
        .to_os_string();
    tmp_name.push(".part");
    let tmp = out.with_file_name(tmp_name);

    let result = (|| -> Result<()> {
        let mut writer = WavWriter::create(&tmp, track_spec())
            .with_context(|| format!("Failed to create {}", tmp.display()))?;
        loop {
            let mut sum = 0i32;
            let mut any = false;
            for reader in &mut readers {
                if let Some(sample) = reader.next() {
                    sum += sample? as i32;
                    any = true;
                }
            }
            if !any {
                break;
            }
            writer.write_sample(sum.clamp(-(i16::MAX as i32), i16::MAX as i32) as i16)?;
        }
        writer.finalize()?;
        fs::rename(&tmp, out)
            .with_context(|| format!("Failed to move the mix to {}", out.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_track(path: &Path, samples: &[f32]) {
        let mut writer = TrackWriter::create(path).unwrap();
        writer.push(samples).unwrap();
        writer.finish().unwrap();
    }

    #[test]
    fn track_writer_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("microphone.wav");
        let mut writer = TrackWriter::create(&path).unwrap();
        writer.push(&[0.5, -0.5, 1.0]).unwrap();
        writer.flush().unwrap();
        writer.pad_silence(2).unwrap();
        writer.push(&[2.0, -2.0]).unwrap();
        assert_eq!(writer.samples(), 7);
        assert_eq!(writer.finish().unwrap(), 7);

        let samples = read_track(&path).unwrap();
        assert_eq!(samples.len(), 7);
        assert!((samples[0] - 0.5).abs() < 1e-3);
        assert!((samples[1] + 0.5).abs() < 1e-3);
        assert_eq!(samples[2], 1.0);
        assert_eq!(&samples[3..5], &[0.0, 0.0]);
        // Out-of-range input saturates.
        assert_eq!(samples[5], 1.0);
        assert_eq!(samples[6], -1.0);
    }

    #[test]
    fn read_track_refuses_another_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stereo.wav");
        let spec = WavSpec {
            channels: 2,
            ..track_spec()
        };
        let mut writer = WavWriter::create(&path, spec).unwrap();
        writer.write_sample(0i16).unwrap();
        writer.write_sample(0i16).unwrap();
        writer.finalize().unwrap();
        assert!(read_track(&path).is_err());
    }

    #[test]
    fn repair_wav_restores_samples_past_a_stale_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("system.wav");
        let samples: Vec<f32> = (0..150).map(|i| i as f32 / 1000.0).collect();
        write_track(&path, &samples);

        // As if the last checkpoint saw only 100 samples, then the app died
        // halfway through writing one more.
        let mut bytes = fs::read(&path).unwrap();
        assert_eq!(&bytes[36..40], b"data");
        bytes[40..44].copy_from_slice(&(100u32 * 2).to_le_bytes());
        bytes[4..8].copy_from_slice(&(36u32 + 100 * 2).to_le_bytes());
        bytes.push(0x7f);
        fs::write(&path, &bytes).unwrap();
        assert_eq!(WavReader::open(&path).unwrap().len(), 100);

        assert_eq!(repair_wav(&path).unwrap(), 150);
        assert_eq!(fs::metadata(&path).unwrap().len(), 44 + 150 * 2);
        let repaired = read_track(&path).unwrap();
        assert_eq!(repaired.len(), 150);
        assert!((repaired[149] - 0.149).abs() < 1e-3);

        // A complete file is left as it is.
        assert_eq!(repair_wav(&path).unwrap(), 150);
        assert_eq!(read_track(&path).unwrap(), repaired);
    }

    #[test]
    fn repair_wav_handles_a_header_with_no_data_yet() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("microphone.wav");
        write_track(&path, &[]);
        let mut bytes = fs::read(&path).unwrap();
        bytes.extend_from_slice(&[1, 0, 2, 0, 3]);
        fs::write(&path, &bytes).unwrap();
        assert_eq!(repair_wav(&path).unwrap(), 2);
        assert_eq!(read_track(&path).unwrap().len(), 2);
    }

    #[test]
    fn repair_wav_rejects_a_file_that_is_not_a_wav() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("junk.wav");
        fs::write(&path, b"not a wav").unwrap();
        assert!(repair_wav(&path).is_err());
    }

    #[test]
    fn mix_tracks_pads_the_shorter_track_and_clamps() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("microphone.wav");
        let b = dir.path().join("system.wav");
        write_track(&a, &[0.25, 0.75, -0.75, 0.5]);
        write_track(&b, &[0.25, 0.75, -0.75]);
        let out = dir.path().join("playback.wav");
        mix_tracks(&[a, b], &out).unwrap();

        let mixed = read_track(&out).unwrap();
        assert_eq!(mixed.len(), 4);
        assert!((mixed[0] - 0.5).abs() < 1e-3);
        assert_eq!(mixed[1], 1.0);
        assert_eq!(mixed[2], -1.0);
        assert!((mixed[3] - 0.5).abs() < 1e-3);
        assert!(!dir.path().join("playback.wav.part").exists());
    }

    #[test]
    fn mix_tracks_replaces_an_existing_mix() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("microphone.wav");
        write_track(&a, &[0.5; 10]);
        let out = dir.path().join("playback.wav");
        write_track(&out, &[0.1; 3]);
        mix_tracks(&[a], &out).unwrap();
        assert_eq!(read_track(&out).unwrap().len(), 10);
    }

    #[test]
    fn mix_tracks_fails_without_inputs() {
        let dir = tempfile::tempdir().unwrap();
        assert!(mix_tracks(&[], &dir.path().join("playback.wav")).is_err());
    }
}
