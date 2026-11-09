//! A meeting taken out of the app: its transcript as a text, subtitle or JSON
//! file, and its audio as one WAV with both sides mixed (the same file the
//! player uses).

use super::capture::{MICROPHONE_TRACK, SYSTEM_TRACK};
use super::store::{Meeting, MeetingSegment, MeetingTranscriptFormat, PLAYBACK_FILE};
use super::wav::mix_tracks;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Local, TimeZone, Utc};
use serde::Serialize;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

/// The transcript in `format`. `names` maps each speaker key to the name to
/// print; a key without one prints as itself.
pub fn render_transcript(
    meeting: &Meeting,
    segments: &[MeetingSegment],
    format: MeetingTranscriptFormat,
    names: &HashMap<String, String>,
) -> String {
    let name = |key: &str| names.get(key).map_or(key, String::as_str).to_string();
    let mut out = String::new();
    match format {
        MeetingTranscriptFormat::Txt => {
            for s in segments {
                let _ = write!(
                    out,
                    "{} ({})\n{}\n\n",
                    name(&s.speaker),
                    clock(s.start_ms),
                    s.text
                );
            }
        }
        MeetingTranscriptFormat::Md => {
            let _ = write!(
                out,
                "# {}\n\n{} \u{b7} {}\n\n",
                title(meeting),
                local_time(meeting.started_at).format("%Y-%m-%d %H:%M"),
                clock(meeting.duration_ms)
            );
            for s in segments {
                let _ = write!(
                    out,
                    "**{}** ({}): {}\n\n",
                    name(&s.speaker),
                    clock(s.start_ms),
                    s.text
                );
            }
        }
        MeetingTranscriptFormat::Srt => {
            for (i, s) in segments.iter().enumerate() {
                let (start, end) = cue_span(s);
                let _ = write!(
                    out,
                    "{}\n{} --> {}\n{}: {}\n\n",
                    i + 1,
                    cue_time(start, ','),
                    cue_time(end, ','),
                    name(&s.speaker),
                    s.text
                );
            }
        }
        MeetingTranscriptFormat::Vtt => {
            out.push_str("WEBVTT\n\n");
            for s in segments {
                let (start, end) = cue_span(s);
                let _ = write!(
                    out,
                    "{} --> {}\n<v {}>{}\n\n",
                    cue_time(start, '.'),
                    cue_time(end, '.'),
                    vtt_escape(&name(&s.speaker)).replace('>', "&gt;"),
                    vtt_escape(&s.text)
                );
            }
        }
        MeetingTranscriptFormat::Json => {
            let json = JsonTranscript {
                title: title(meeting),
                started_at: local_time(meeting.started_at).to_rfc3339(),
                duration_ms: meeting.duration_ms,
                model_id: meeting.model_id.as_deref(),
                language: meeting.language.as_deref(),
                speakers: meeting
                    .speakers
                    .iter()
                    .map(|sp| JsonSpeaker {
                        key: &sp.key,
                        name: name(&sp.key),
                        talk_ms: sp.talk_ms,
                    })
                    .collect(),
                segments: segments
                    .iter()
                    .map(|s| JsonSegment {
                        speaker: &s.speaker,
                        name: name(&s.speaker),
                        start_ms: s.start_ms,
                        end_ms: s.end_ms,
                        text: &s.text,
                    })
                    .collect(),
            };
            out = serde_json::to_string_pretty(&json).expect("a transcript always serializes");
            out.push('\n');
        }
    }
    out
}

#[derive(Serialize)]
struct JsonTranscript<'a> {
    title: String,
    started_at: String,
    duration_ms: i64,
    model_id: Option<&'a str>,
    language: Option<&'a str>,
    speakers: Vec<JsonSpeaker<'a>>,
    segments: Vec<JsonSegment<'a>>,
}

#[derive(Serialize)]
struct JsonSpeaker<'a> {
    key: &'a str,
    name: String,
    talk_ms: i64,
}

#[derive(Serialize)]
struct JsonSegment<'a> {
    speaker: &'a str,
    name: String,
    start_ms: i64,
    end_ms: i64,
    text: &'a str,
}

/// The user's title, or one made from the start time (what the UI shows is
/// localized; a file needs something stable).
fn title(meeting: &Meeting) -> String {
    let title = meeting.title.trim();
    if title.is_empty() {
        format!(
            "Meeting {}",
            local_time(meeting.started_at).format("%Y-%m-%d %H:%M")
        )
    } else {
        title.to_string()
    }
}

fn local_time(unix_secs: i64) -> DateTime<Local> {
    Utc.timestamp_opt(unix_secs, 0)
        .single()
        .unwrap_or_default()
        .with_timezone(&Local)
}

/// `HH:MM:SS`.
fn clock(ms: i64) -> String {
    let secs = ms.max(0) / 1000;
    format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

/// `HH:MM:SS,mmm` (SRT) or `HH:MM:SS.mmm` (WebVTT).
fn cue_time(ms: i64, separator: char) -> String {
    let ms = ms.max(0);
    format!("{}{separator}{:03}", clock(ms), ms % 1000)
}

/// A cue never ends before it starts; a zero-length utterance (a model
/// without timestamps can give one) shows for a second.
fn cue_span(segment: &MeetingSegment) -> (i64, i64) {
    let start = segment.start_ms.max(0);
    let end = if segment.end_ms > start {
        segment.end_ms
    } else {
        start + 1000
    };
    (start, end)
}

/// WebVTT cue text treats `&` and `<` as markup.
fn vtt_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;")
}

/// `folder/playback.wav`, mixed from the meeting's tracks when it isn't
/// there yet. The caller checks the audio is kept.
pub fn playback_path(folder: &Path) -> Result<PathBuf> {
    let playback = folder.join(PLAYBACK_FILE);
    if playback.is_file() {
        return Ok(playback);
    }
    let tracks: Vec<PathBuf> = [MICROPHONE_TRACK, SYSTEM_TRACK]
        .iter()
        .map(|name| folder.join(name))
        .filter(|path| path.is_file())
        .collect();
    if tracks.is_empty() {
        bail!("This meeting has no audio");
    }
    mix_tracks(&tracks, &playback).context("Couldn't mix the meeting's audio")?;
    Ok(playback)
}

/// Writes the mixed audio to `dest`, making it first when missing.
pub fn export_audio(folder: &Path, dest: &Path) -> Result<()> {
    let playback = playback_path(folder)?;
    fs::copy(&playback, dest).with_context(|| format!("Couldn't write {}", dest.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managers::meeting::store::{MeetingAudio, MeetingSpeaker, MeetingStatus};

    fn meeting(title: &str) -> Meeting {
        Meeting {
            id: 1,
            title: title.to_string(),
            started_at: 0,
            duration_ms: 3_725_000,
            status: MeetingStatus::Ready,
            progress: None,
            error: None,
            model_id: Some("parakeet".to_string()),
            language: Some("en".to_string()),
            has_microphone: true,
            has_system_audio: true,
            audio: MeetingAudio::Kept,
            recovered: false,
            speakers: vec![MeetingSpeaker {
                key: "me".to_string(),
                name: None,
                talk_ms: 1500,
            }],
        }
    }

    fn segment(speaker: &str, start_ms: i64, end_ms: i64, text: &str) -> MeetingSegment {
        MeetingSegment {
            speaker: speaker.to_string(),
            start_ms,
            end_ms,
            text: text.to_string(),
        }
    }

    fn names() -> HashMap<String, String> {
        HashMap::from([("me".to_string(), "Azain".to_string())])
    }

    #[test]
    fn cue_times_use_each_formats_separator() {
        assert_eq!(cue_time(3_723_045, ','), "01:02:03,045");
        assert_eq!(cue_time(3_723_045, '.'), "01:02:03.045");
        assert_eq!(cue_time(-5, ','), "00:00:00,000");
    }

    #[test]
    fn srt_numbers_cues_and_never_ends_a_cue_before_it_starts() {
        let srt = render_transcript(
            &meeting("Sync"),
            &[
                segment("me", 1_000, 2_500, "Hello"),
                segment("s1", 3_000, 3_000, "Hi"),
            ],
            MeetingTranscriptFormat::Srt,
            &names(),
        );
        assert_eq!(
            srt,
            "1\n00:00:01,000 --> 00:00:02,500\nAzain: Hello\n\n\
             2\n00:00:03,000 --> 00:00:04,000\ns1: Hi\n\n"
        );
    }

    #[test]
    fn vtt_escapes_markup_in_names_and_text() {
        let names = HashMap::from([("s1".to_string(), "A<B>".to_string())]);
        let vtt = render_transcript(
            &meeting("Sync"),
            &[segment("s1", 0, 1_000, "x < y & z")],
            MeetingTranscriptFormat::Vtt,
            &names,
        );
        assert_eq!(
            vtt,
            "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\n<v A&lt;B&gt;>x &lt; y &amp; z\n\n"
        );
    }

    #[test]
    fn an_untitled_meeting_is_named_after_its_start() {
        let md = render_transcript(&meeting("  "), &[], MeetingTranscriptFormat::Md, &names());
        assert!(md.starts_with("# Meeting "), "{md}");
        assert!(md.contains("01:02:05"), "{md}");
    }

    #[test]
    fn json_prints_names_for_speakers_and_segments() {
        let json = render_transcript(
            &meeting("Sync"),
            &[segment("me", 0, 1_000, "Hello")],
            MeetingTranscriptFormat::Json,
            &names(),
        );
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["title"], "Sync");
        assert_eq!(value["speakers"][0]["name"], "Azain");
        assert_eq!(value["segments"][0]["name"], "Azain");
        assert_eq!(value["segments"][0]["text"], "Hello");
    }

    #[test]
    fn playback_needs_a_track() {
        let dir = tempfile::tempdir().unwrap();
        assert!(playback_path(dir.path()).is_err());
    }
}
