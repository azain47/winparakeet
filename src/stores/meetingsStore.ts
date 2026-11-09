import { create } from "zustand";
import {
  commands,
  events,
  type Meeting,
  type MeetingRecording,
} from "@/bindings";

/** Input levels of the meeting being recorded, 0-1. */
export interface MeetingInputLevels {
  microphone: number;
  system_audio: number;
}

// Every meeting outside the Bin, newest first, and the one being recorded.
// The backend's events keep both current, so pages read them from here.
interface MeetingsStore {
  meetings: Meeting[];
  /** False until the first list arrives. */
  loaded: boolean;
  recording: MeetingRecording | null;
  levels: MeetingInputLevels;
  /**
   * The recording's elapsed time as last reported, and when (performance.now()),
   * so a timer can count on between reports.
   */
  elapsed: { ms: number; at: number };
  initialized: boolean;
  initialize: () => Promise<void>;
}

const SILENT: MeetingInputLevels = { microphone: 0, system_audio: 0 };

const newestFirst = (a: Meeting, b: Meeting) =>
  b.started_at - a.started_at || b.id - a.id;

const upsert = (meetings: Meeting[], meeting: Meeting) =>
  [...meetings.filter((m) => m.id !== meeting.id), meeting].sort(newestFirst);

export const useMeetingsStore = create<MeetingsStore>()((set, get) => ({
  meetings: [],
  loaded: false,
  recording: null,
  levels: SILENT,
  elapsed: { ms: 0, at: 0 },
  initialized: false,

  initialize: async () => {
    if (get().initialized) return;
    set({ initialized: true });

    // Listen first, so nothing that changes meanwhile is missed; an event is
    // newer than the snapshots below, which may have been read before it.
    const reported = new Set<number>();
    let recordingReported = false;
    let hydrating = true;

    await Promise.all([
      events.meetingUpdate.listen(({ payload }) => {
        if (payload.action === "upserted") {
          if (hydrating) reported.add(payload.meeting.id);
          set((state) => ({
            meetings: upsert(state.meetings, payload.meeting),
          }));
        } else {
          if (hydrating) reported.add(payload.id);
          set((state) => ({
            meetings: state.meetings.filter((m) => m.id !== payload.id),
          }));
        }
      }),
      events.meetingRecordingChanged.listen(({ payload }) => {
        recordingReported = true;
        const { recording } = payload;
        set({
          recording,
          ...(recording
            ? { elapsed: { ms: recording.elapsed_ms, at: performance.now() } }
            : { levels: SILENT }),
        });
      }),
      events.meetingLevels.listen(({ payload }) => {
        if (get().recording?.meeting_id !== payload.meeting_id) return;
        set({
          levels: {
            microphone: payload.microphone,
            system_audio: payload.system_audio,
          },
          elapsed: { ms: payload.elapsed_ms, at: performance.now() },
        });
      }),
    ]);

    try {
      const [list, recording] = await Promise.all([
        commands.getMeetings(),
        commands.getMeetingRecording(),
      ]);
      if (list.status === "ok") {
        set((state) => ({
          meetings: list.data
            .filter((m) => !reported.has(m.id))
            .reduce(upsert, state.meetings),
        }));
      } else {
        console.error("Failed to load meetings:", list.error);
      }
      if (recording.status === "ok" && !recordingReported) {
        const current = recording.data;
        set({
          recording: current,
          elapsed: current
            ? { ms: current.elapsed_ms, at: performance.now() }
            : get().elapsed,
        });
      }
    } catch (error) {
      console.error("Failed to load meetings:", error);
    } finally {
      hydrating = false;
      set({ loaded: true });
    }
  },
}));

/** Whether a meeting is being recorded right now. */
export const useMeetingRecordingActive = () =>
  useMeetingsStore((state) => state.recording !== null);
