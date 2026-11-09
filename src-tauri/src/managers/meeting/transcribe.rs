//! A meeting's transcript from its tracks' timed units. Each side is grouped
//! into utterances; a side the diarizer found two or more people on is split
//! by who spoke. The microphone's echo of the other side is dropped first, so
//! it never passes for someone in the room.

use log::info;
use std::collections::{HashMap, HashSet};

use super::store::MeetingSegment;
use crate::audio_toolkit::chunking::join_transcripts;
use crate::managers::transcription::TimedUnit;

/// The microphone's side when one person speaks on it.
pub const ME: &str = "me";
/// The other side when it isn't split into speakers.
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
/// A microphone speaker with less speech than this is a diarization
/// fragment of someone else: their words go to the main speaker.
const MIN_MICROPHONE_SPEAKER_MS: i64 = 5_000;

/// A diarized speaker turn of one track; `speaker` as the diarizer numbers
/// them (per track).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Turn {
    pub start_ms: i64,
    pub end_ms: i64,
    pub speaker: i32,
}

/// One utterance of the transcript, with the track it came from: the caller
/// polishes its text with that track's settings.
#[derive(Clone, Debug, PartialEq)]
pub struct Utterance {
    pub from_microphone: bool,
    pub segment: MeetingSegment,
}

/// The transcript's utterances, by start, the microphone's first on a tie.
/// Texts are as transcribed. `me` and `them` are the microphone's and system
/// audio's units (empty for a side not recorded); `me_turns` and
/// `them_turns` their diarization (empty when not run).
///
/// Speakers: a side with one diarized speaker (or none) is `me` / `them`.
/// Otherwise every diarized speaker of either side gets `s1`, `s2`, ... in
/// order of first appearance in the transcript.
pub fn utterances(
    me: &[TimedUnit],
    them: &[TimedUnit],
    me_turns: &[Turn],
    them_turns: &[Turn],
) -> Vec<Utterance> {
    // The microphone undiarized first, exactly as with one person on it,
    // and its echo of the other side dropped.
    let mut mine = group(me.iter().map(|unit| (unit, None)));
    if !them.is_empty() {
        let before = mine.len();
        mine.retain(|group| !is_echo(group, them));
        let dropped = before - mine.len();
        if dropped > 0 {
            info!(
                "Dropped {} microphone utterances that echoed the other side",
                dropped
            );
        }
    }
    // Then who in the room said the rest.
    let kept: Vec<&TimedUnit> = mine.iter().flat_map(|g| g.units.iter().copied()).collect();
    if let Some(speakers) = microphone_speakers(&kept, me_turns) {
        mine = group(kept.into_iter().zip(speakers.into_iter().map(Some)));
    }

    let assigned: Vec<Option<i32>> = them.iter().map(|u| speaker_for(u, them_turns)).collect();
    let distinct: HashSet<i32> = assigned.iter().flatten().copied().collect();
    let theirs = if distinct.len() >= 2 {
        group(them.iter().zip(assigned))
    } else {
        group(them.iter().map(|unit| (unit, None)))
    };

    let mut all: Vec<(bool, Group)> = mine
        .into_iter()
        .map(|g| (true, g))
        .chain(theirs.into_iter().map(|g| (false, g)))
        .collect();
    all.sort_by(|(a_mic, a), (b_mic, b)| {
        a.start_ms.cmp(&b.start_ms).then_with(|| b_mic.cmp(a_mic))
    });

    // One numbering for both sides, by first appearance.
    let mut keys: HashMap<(bool, i32), String> = HashMap::new();
    all.into_iter()
        .map(|(from_microphone, group)| {
            let speaker = match group.speaker {
                Some(id) => {
                    let next = format!("s{}", keys.len() + 1);
                    keys.entry((from_microphone, id)).or_insert(next).clone()
                }
                None if from_microphone => ME.to_string(),
                None => THEM.to_string(),
            };
            Utterance {
                from_microphone,
                segment: group.into_segment(speaker),
            }
        })
        .collect()
}

/// The diarized speaker of each microphone unit (echo already left out),
/// or `None` when the microphone had one speaker: then the side stays `me`.
/// Speakers with less than [`MIN_MICROPHONE_SPEAKER_MS`] of speech, and
/// units outside every turn, go to the speaker with the most speech.
fn microphone_speakers(units: &[&TimedUnit], turns: &[Turn]) -> Option<Vec<i32>> {
    let assigned: Vec<Option<i32>> = units.iter().map(|u| speaker_for(u, turns)).collect();
    let mut speech: HashMap<i32, i64> = HashMap::new();
    for (unit, speaker) in units.iter().zip(&assigned) {
        if let Some(speaker) = speaker {
            *speech.entry(*speaker).or_default() += (unit.end_ms - unit.start_ms).max(0);
        }
    }
    let real: HashSet<i32> = speech
        .iter()
        .filter(|(_, ms)| **ms >= MIN_MICROPHONE_SPEAKER_MS)
        .map(|(id, _)| *id)
        .collect();
    if real.len() < 2 {
        return None;
    }
    // Most speech; the lower id on a tie, so the choice is stable.
    let main = speech
        .iter()
        .max_by_key(|(id, ms)| (**ms, -**id))
        .map(|(id, _)| *id)?;
    Some(
        assigned
            .into_iter()
            .map(|speaker| match speaker {
                Some(speaker) if real.contains(&speaker) => speaker,
                _ => main,
            })
            .collect(),
    )
}

/// Units being grouped into one utterance.
struct Group<'a> {
    speaker: Option<i32>,
    start_ms: i64,
    end_ms: i64,
    units: Vec<&'a TimedUnit>,
}

impl Group<'_> {
    fn text(&self) -> String {
        join_units(self.units.iter().map(|unit| unit.text.as_str()))
    }

    fn into_segment(self, speaker: String) -> MeetingSegment {
        MeetingSegment {
            speaker,
            start_ms: self.start_ms,
            end_ms: self.end_ms,
            text: self.text(),
        }
    }

    /// Whether `unit` (of `speaker`) starts a new utterance.
    fn breaks_before(&self, unit: &TimedUnit, speaker: Option<i32>) -> bool {
        speaker != self.speaker
            || unit.start_ms - self.end_ms >= MAX_GAP_MS
            || unit.end_ms - self.start_ms > MAX_UTTERANCE_MS
            || (self.end_ms - self.start_ms >= SENTENCE_BREAK_AFTER_MS
                && self
                    .units
                    .last()
                    .is_some_and(|last| ends_sentence(&last.text)))
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
                group.units.push(unit);
            }
            _ => groups.push(Group {
                speaker,
                start_ms: unit.start_ms,
                end_ms: unit.end_ms,
                units: vec![unit],
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
fn join_units<'a>(texts: impl Iterator<Item = &'a str>) -> String {
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
fn is_echo(utterance: &Group, them: &[TimedUnit]) -> bool {
    let words = normalized_words(&utterance.text());
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

    /// `count` words, one every 500 ms from `start_ms`, named `{name}{i}`.
    fn words(start_ms: i64, count: i64, name: &str) -> Vec<TimedUnit> {
        (0..count)
            .map(|i| {
                let start = start_ms + i * 500;
                unit(start, start + 400, &format!("{name}{i}"))
            })
            .collect()
    }

    fn turn(start_ms: i64, end_ms: i64, speaker: i32) -> Turn {
        Turn {
            start_ms,
            end_ms,
            speaker,
        }
    }

    fn run(me: &[TimedUnit], them: &[TimedUnit], turns: &[Turn]) -> Vec<MeetingSegment> {
        both(me, them, &[], turns)
    }

    fn both(
        me: &[TimedUnit],
        them: &[TimedUnit],
        me_turns: &[Turn],
        them_turns: &[Turn],
    ) -> Vec<MeetingSegment> {
        utterances(me, them, me_turns, them_turns)
            .into_iter()
            .map(|u| u.segment)
            .collect()
    }

    fn texts(segments: &[MeetingSegment]) -> Vec<(&str, &str)> {
        segments
            .iter()
            .map(|s| (s.speaker.as_str(), s.text.as_str()))
            .collect()
    }

    fn speakers(segments: &[MeetingSegment]) -> Vec<&str> {
        segments.iter().map(|s| s.speaker.as_str()).collect()
    }

    #[test]
    fn a_pause_of_a_second_ends_an_utterance() {
        let me = [
            unit(0, 400, "hello"),
            unit(500, 900, "there"),
            unit(1_900, 2_300, "again"),
        ];
        let out = run(&me, &[], &[]);
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
        let out = run(&me, &[], &[]);
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
        assert_eq!(run(&me, &[], &[])[0].text, "Hi, you");
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
        let out = run(&[], &them, &turns);
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
        let out = run(&[], &them, &[turn(0, 450, 1), turn(450, 1_000, 2)]);
        assert_eq!(texts(&out), vec![("s1", "a"), ("s2", "b")]);
    }

    #[test]
    fn one_diarized_speaker_on_the_other_side_stays_them() {
        let them = words(0, 4, "w");
        let out = run(&[], &them, &[turn(0, 2_000, 1)]);
        assert_eq!(speakers(&out), vec!["them"]);
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
        let out = run(&me, &them, &[]);
        assert_eq!(
            texts(&out),
            vec![("them", "Can you hear me?"), ("me", "yes I can")]
        );

        // Short replies are never taken for an echo.
        let out = run(&[unit(1_000, 1_300, "you")], &them, &[]);
        assert_eq!(out[0].speaker, "me");
    }

    #[test]
    fn the_microphone_comes_first_on_a_tie() {
        let out = run(&[unit(0, 100, "mine")], &[unit(0, 100, "theirs")], &[]);
        assert_eq!(texts(&out), vec![("me", "mine"), ("them", "theirs")]);
    }

    #[test]
    fn one_speaker_on_the_microphone_stays_me() {
        let me = words(0, 40, "w");
        let diarized = both(&me, &[], &[turn(0, 20_000, 1)], &[]);
        assert_eq!(diarized, both(&me, &[], &[], &[]));
        assert!(diarized.iter().all(|s| s.speaker == "me"));
    }

    #[test]
    fn two_people_on_the_microphone_are_split() {
        let mut me = words(0, 20, "a");
        me.extend(words(12_000, 20, "b"));
        let turns = [turn(0, 10_000, 1), turn(12_000, 22_000, 2)];
        let out = both(&me, &[], &turns, &[]);
        assert_eq!(speakers(&out), vec!["s1", "s2"]);
        assert!(out[1].text.starts_with("b0"));
    }

    #[test]
    fn a_short_second_microphone_speaker_folds_into_the_main_one() {
        // 20 s of one voice; the diarizer splits 2 s of it off.
        let me = words(0, 40, "w");
        let turns = [
            turn(0, 9_000, 1),
            turn(9_000, 11_000, 2),
            turn(11_000, 20_000, 1),
        ];
        let out = both(&me, &[], &turns, &[]);
        assert_eq!(out, both(&me, &[], &[], &[]));
        assert!(out.iter().all(|s| s.speaker == "me"));
    }

    #[test]
    fn echo_never_makes_a_microphone_speaker() {
        // 10 s of me, then 6 s of the other side echoed on the microphone,
        // which the diarizer takes for a second person.
        let mut me = words(0, 20, "a");
        let them = words(30_000, 12, "e");
        me.extend(them.iter().cloned());
        let me_turns = [turn(0, 10_000, 1), turn(30_000, 36_000, 2)];
        let out = both(&me, &them, &me_turns, &[]);
        assert_eq!(speakers(&out), vec!["me", "them"]);
    }

    #[test]
    fn speakers_of_both_sides_share_one_numbering_by_first_appearance() {
        let mut me = words(0, 20, "a");
        me.extend(words(20_000, 20, "b"));
        let me_turns = [turn(0, 10_000, 1), turn(20_000, 30_000, 2)];
        let mut them = words(12_000, 4, "c");
        them.extend(words(15_000, 4, "d"));
        let them_turns = [turn(12_000, 14_500, 1), turn(14_500, 17_000, 2)];
        let out = both(&me, &them, &me_turns, &them_turns);
        assert_eq!(speakers(&out), vec!["s1", "s2", "s3", "s4"]);
        assert!(out[1].text.starts_with('c'));
        assert!(out[3].text.starts_with('b'));
    }

    #[test]
    fn a_microphone_only_meeting_with_three_people() {
        // 5.6 s of speech each (14 words of 400 ms), over the 5 s floor.
        let mut me = words(0, 14, "a");
        me.extend(words(8_000, 14, "b"));
        me.extend(words(16_000, 14, "c"));
        me.extend(words(24_000, 14, "a"));
        // The diarizer numbers them 3, 1, 2.
        let turns = [
            turn(0, 7_000, 3),
            turn(8_000, 15_000, 1),
            turn(16_000, 23_000, 2),
            turn(24_000, 31_000, 3),
        ];
        let out = both(&me, &[], &turns, &[]);
        assert_eq!(speakers(&out), vec!["s1", "s2", "s3", "s1"]);
    }
}
