//! A meeting's transcript from its tracks' timed units: the microphone's
//! units grouped into `me` utterances, the system audio's split by who spoke
//! (diarization) and grouped likewise, and the microphone's echo of the other
//! side dropped.

use log::info;
use std::collections::{HashMap, HashSet};

use super::store::MeetingSegment;
use crate::audio_toolkit::chunking::join_transcripts;
use crate::managers::transcription::TimedUnit;

/// The microphone's speaker.
pub const ME: &str = "me";
/// The other side, when it isn't split into speakers.
pub const THEM: &str = "them";

/// A pause this long ends an utterance.
const MAX_GAP_MS: i64 = 1_000;
/// No utterance runs longer.
const MAX_UTTERANCE_MS: i64 = 30_000;
/// An utterance this long ends at its next sentence end.
const SENTENCE_BREAK_AFTER_MS: i64 = 15_000;
/// A word outside every turn goes to the nearest turn this close.
const NEAREST_TURN_MS: i64 = 1_000;
/// How far around a microphone utterance the other side is searched for it.
const BLEED_WINDOW_MS: i64 = 1_500;
/// Shorter microphone utterances are never taken for an echo.
const BLEED_MIN_WORDS: usize = 3;
/// The share of an utterance's words heard from the other side that makes
/// it an echo.
const BLEED_SHARE: f64 = 0.6;

/// A diarized speaker turn of the system track; `speaker` as the diarizer
/// numbers them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Turn {
    pub start_ms: i64,
    pub end_ms: i64,
    pub speaker: i32,
}

/// The transcript's utterances, by start, `me` first on a tie. Texts are as
/// transcribed; the caller polishes them per side. `me` and `them` are the
/// tracks' units (empty for a side not recorded); `turns` the system track's
/// diarization (empty when not run).
pub fn utterances(me: &[TimedUnit], them: &[TimedUnit], turns: &[Turn]) -> Vec<MeetingSegment> {
    let mine = group(me.iter().map(|unit| (unit, None)));

    let theirs = group(them.iter().map(|unit| (unit, speaker_for(unit, turns))));
    // Diarizer ids become s1..s4 in order of first appearance.
    let mut keys: HashMap<i32, String> = HashMap::new();
    let mut theirs: Vec<MeetingSegment> = theirs
        .into_iter()
        .map(|group| {
            let speaker = match group.speaker {
                Some(id) => {
                    let next = format!("s{}", keys.len() + 1);
                    keys.entry(id).or_insert(next).clone()
                }
                None => THEM.to_string(),
            };
            group.into_segment(speaker)
        })
        .collect();

    let mut mine: Vec<MeetingSegment> = mine
        .into_iter()
        .map(|group| group.into_segment(ME.to_string()))
        .collect();
    if !them.is_empty() {
        let before = mine.len();
        mine.retain(|utterance| !is_echo(utterance, them));
        let dropped = before - mine.len();
        if dropped > 0 {
            info!(
                "Dropped {} microphone utterances that echoed the other side",
                dropped
            );
        }
    }

    mine.append(&mut theirs);
    mine.sort_by(|a, b| {
        a.start_ms
            .cmp(&b.start_ms)
            .then_with(|| (a.speaker != ME).cmp(&(b.speaker != ME)))
    });
    mine
}

/// Units being grouped into one utterance.
struct Group<'a> {
    speaker: Option<i32>,
    start_ms: i64,
    end_ms: i64,
    texts: Vec<&'a str>,
}

impl Group<'_> {
    fn into_segment(self, speaker: String) -> MeetingSegment {
        MeetingSegment {
            speaker,
            start_ms: self.start_ms,
            end_ms: self.end_ms,
            text: join_units(&self.texts),
        }
    }

    /// Whether `unit` (of `speaker`) starts a new utterance.
    fn breaks_before(&self, unit: &TimedUnit, speaker: Option<i32>) -> bool {
        speaker != self.speaker
            || unit.start_ms - self.end_ms >= MAX_GAP_MS
            || unit.end_ms - self.start_ms > MAX_UTTERANCE_MS
            || (self.end_ms - self.start_ms >= SENTENCE_BREAK_AFTER_MS
                && self.texts.last().is_some_and(|text| ends_sentence(text)))
    }
}

/// Groups units, in order of start, into utterances.
fn group<'a>(units: impl Iterator<Item = (&'a TimedUnit, Option<i32>)>) -> Vec<Group<'a>> {
    let mut units: Vec<(&TimedUnit, Option<i32>)> = units.collect();
    units.sort_by_key(|(unit, _)| unit.start_ms);
    let mut groups: Vec<Group> = Vec::new();
    for (unit, speaker) in units {
        match groups.last_mut() {
            Some(group) if !group.breaks_before(unit, speaker) => {
                group.end_ms = group.end_ms.max(unit.end_ms);
                group.texts.push(&unit.text);
            }
            _ => groups.push(Group {
                speaker,
                start_ms: unit.start_ms,
                end_ms: unit.end_ms,
                texts: vec![&unit.text],
            }),
        }
    }
    groups
}

fn ends_sentence(text: &str) -> bool {
    text.trim_end()
        .ends_with(['.', '?', '!', '…', '。', '？', '！'])
}

/// Joins units' texts with spaces, except before punctuation and around
/// unspaced scripts.
fn join_units(texts: &[&str]) -> String {
    let mut joined = String::new();
    for text in texts {
        let text = text.trim();
        if text.starts_with([',', '.', ';', ':', '!', '?', '%', ')', ']', '}', '…']) {
            joined.push_str(text);
        } else {
            joined = join_transcripts(&[joined.as_str(), text]);
        }
    }
    joined
}

/// The diarized speaker of a unit: the turn overlapping it most, a turn
/// holding its midpoint first; else the nearest turn within a second; else
/// none.
fn speaker_for(unit: &TimedUnit, turns: &[Turn]) -> Option<i32> {
    let mid = (unit.start_ms + unit.end_ms) / 2;
    let overlapping = turns
        .iter()
        .filter_map(|turn| {
            let overlap = unit.end_ms.min(turn.end_ms) - unit.start_ms.max(turn.start_ms);
            let holds_mid = turn.start_ms <= mid && mid < turn.end_ms;
            (overlap > 0 || holds_mid).then_some(((holds_mid, overlap.max(0)), turn.speaker))
        })
        .max_by_key(|(rank, _)| *rank)
        .map(|(_, speaker)| speaker);
    overlapping.or_else(|| {
        turns
            .iter()
            .map(|turn| {
                let distance = if unit.end_ms <= turn.start_ms {
                    turn.start_ms - unit.end_ms
                } else {
                    unit.start_ms - turn.end_ms
                };
                (distance, turn.speaker)
            })
            .filter(|(distance, _)| *distance <= NEAREST_TURN_MS)
            .min_by_key(|(distance, _)| *distance)
            .map(|(_, speaker)| speaker)
    })
}

/// Lowercase letters and digits of each word.
fn normalized_words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|word| !word.is_empty())
        .collect()
}

/// Whether a microphone utterance is the other side heard through the
/// speakers: most of its words are said by them around the same time.
fn is_echo(utterance: &MeetingSegment, them: &[TimedUnit]) -> bool {
    let words = normalized_words(&utterance.text);
    if words.len() < BLEED_MIN_WORDS {
        return false;
    }
    let from = utterance.start_ms - BLEED_WINDOW_MS;
    let to = utterance.end_ms + BLEED_WINDOW_MS;
    let heard: HashSet<String> = them
        .iter()
        .filter(|unit| unit.end_ms >= from && unit.start_ms <= to)
        .flat_map(|unit| normalized_words(&unit.text))
        .collect();
    let echoed = words.iter().filter(|word| heard.contains(*word)).count();
    echoed as f64 >= BLEED_SHARE * words.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(start_ms: i64, end_ms: i64, text: &str) -> TimedUnit {
        TimedUnit {
            start_ms,
            end_ms,
            text: text.into(),
        }
    }

    fn turn(start_ms: i64, end_ms: i64, speaker: i32) -> Turn {
        Turn {
            start_ms,
            end_ms,
            speaker,
        }
    }

    fn texts(segments: &[MeetingSegment]) -> Vec<(&str, &str)> {
        segments
            .iter()
            .map(|s| (s.speaker.as_str(), s.text.as_str()))
            .collect()
    }

    #[test]
    fn a_pause_of_a_second_ends_an_utterance() {
        let me = [
            unit(0, 400, "hello"),
            unit(500, 900, "there"),
            unit(1_900, 2_300, "again"),
        ];
        let out = utterances(&me, &[], &[]);
        assert_eq!(texts(&out), vec![("me", "hello there"), ("me", "again")]);
        assert_eq!((out[0].start_ms, out[0].end_ms), (0, 900));
    }

    #[test]
    fn long_utterances_break_at_a_sentence_end_then_at_thirty_seconds() {
        // A word every 500 ms for a minute; a sentence ends at 16 s.
        let me: Vec<TimedUnit> = (0..120)
            .map(|i| {
                let text = if i == 32 { "end." } else { "word" };
                unit(i * 500, i * 500 + 400, text)
            })
            .collect();
        let out = utterances(&me, &[], &[]);
        assert_eq!(out[0].end_ms, 16_400);
        assert_eq!(out[1].start_ms, 16_500);
        // No sentence end after: cut before 30 s.
        assert!(out[1].end_ms - out[1].start_ms <= 30_000);
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn punctuation_units_join_without_a_space() {
        let me = [
            unit(0, 100, "Hi"),
            unit(100, 150, ","),
            unit(200, 300, "you"),
        ];
        assert_eq!(utterances(&me, &[], &[])[0].text, "Hi, you");
    }

    #[test]
    fn words_go_to_the_turn_holding_them_and_speakers_are_numbered_by_appearance() {
        let them = [
            unit(0, 400, "first"),
            unit(500, 900, "speaker"),
            unit(1_000, 1_400, "second"),
            unit(1_500, 1_900, "one"),
            unit(5_000, 5_400, "nobody"),
            unit(2_950, 3_000, "near"),
        ];
        // Diarizer id 3 speaks first, then 1; turns come unsorted.
        let turns = [turn(950, 2_000, 1), turn(0, 950, 3)];
        let out = utterances(&[], &them, &turns);
        assert_eq!(
            texts(&out),
            vec![
                ("s1", "first speaker"),
                ("s2", "second one"),
                // After a pause, within a second of the second turn's end.
                ("s2", "near"),
                ("them", "nobody"),
            ]
        );
    }

    #[test]
    fn the_midpoint_wins_over_a_bigger_overlap_elsewhere() {
        let word = unit(0, 1_000, "word");
        // Turn 1 overlaps 450 ms but not the midpoint (500); turn 2 holds it.
        assert_eq!(
            speaker_for(&word, &[turn(-1_000, 450, 1), turn(450, 600, 2)]),
            Some(2)
        );
        assert_eq!(speaker_for(&word, &[turn(2_100, 3_000, 1)]), None);
        assert_eq!(speaker_for(&word, &[turn(1_900, 3_000, 1)]), Some(1));
    }

    #[test]
    fn a_speaker_change_ends_an_utterance() {
        let them = [unit(0, 400, "a"), unit(500, 900, "b")];
        let out = utterances(&[], &them, &[turn(0, 450, 1), turn(450, 1_000, 2)]);
        assert_eq!(texts(&out), vec![("s1", "a"), ("s2", "b")]);
    }

    #[test]
    fn the_microphone_echoing_the_other_side_is_dropped() {
        let them = [
            unit(1_000, 1_300, "Can"),
            unit(1_300, 1_600, "you"),
            unit(1_600, 1_900, "hear"),
            unit(1_900, 2_200, "me?"),
        ];
        let me = [
            // The echo, a little late and partly misheard.
            unit(1_400, 1_700, "can"),
            unit(1_700, 2_000, "you"),
            unit(2_000, 2_300, "here"),
            unit(2_300, 2_600, "me"),
            // A real reply, later.
            unit(5_000, 5_300, "yes"),
            unit(5_300, 5_600, "I"),
            unit(5_600, 5_900, "can"),
        ];
        let out = utterances(&me, &them, &[]);
        assert_eq!(
            texts(&out),
            vec![("them", "Can you hear me?"), ("me", "yes I can")]
        );

        // Short replies are never taken for an echo.
        let out = utterances(&[unit(1_000, 1_300, "you")], &them, &[]);
        assert_eq!(out[0].speaker, "me");
    }

    #[test]
    fn me_comes_first_on_a_tie() {
        let out = utterances(&[unit(0, 100, "mine")], &[unit(0, 100, "theirs")], &[]);
        assert_eq!(texts(&out), vec![("me", "mine"), ("them", "theirs")]);
    }
}
