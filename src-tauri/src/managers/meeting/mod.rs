//! Meetings: a call recorded as two tracks (my microphone, and the system
//! audio of the other side), transcribed after the call with the other side
//! split into speakers.

pub mod capture;
pub mod diarize;
pub mod export;
pub mod store;
pub mod transcribe;
pub mod wav;

pub use store::{
    Meeting, MeetingAudio, MeetingDetail, MeetingSegment, MeetingStatus, MeetingStore,
    MeetingTranscriptFormat, MeetingUpdate,
};
