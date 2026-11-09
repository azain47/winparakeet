import { create } from "zustand";
import {
  commands,
  events,
  type TranscriptionJob,
  type TranscriptionJobStatus,
} from "@/bindings";

export type ActiveJobStatus = Extract<
  TranscriptionJobStatus,
  { state: "queued" } | { state: "running" }
>;

type Jobs = Record<number, ActiveJobStatus>;

// Background transcriptions (files, and History entries transcribed again),
// keyed by History entry id. Only unfinished jobs are kept.
interface TranscriptionJobsStore {
  jobs: Jobs;
  initialized: boolean;
  initialize: () => Promise<void>;
}

const isActive = (status: TranscriptionJobStatus): status is ActiveJobStatus =>
  status.state === "queued" || status.state === "running";

const applyJob = (jobs: Jobs, { entry_id, status }: TranscriptionJob) => {
  const next = { ...jobs };
  if (isActive(status)) {
    next[entry_id] = status;
  } else {
    // Finished, cancelled or failed.
    delete next[entry_id];
  }
  return next;
};

export const useTranscriptionJobsStore = create<TranscriptionJobsStore>()(
  (set, get) => ({
    jobs: {},
    initialized: false,

    initialize: async () => {
      if (get().initialized) return;
      set({ initialized: true });

      // Entries an event reported while the snapshot below was on its way:
      // the event is newer than the snapshot.
      const reported = new Set<number>();
      let hydrating = true;
      await events.transcriptionJob.listen(({ payload }) => {
        if (hydrating) reported.add(payload.entry_id);
        set((state) => ({ jobs: applyJob(state.jobs, payload) }));
        if (payload.status.state === "failed") {
          console.error(
            `Transcription of entry ${payload.entry_id} failed:`,
            payload.status,
          );
        }
      });

      // Listening first, so no job that starts meanwhile is missed; jobs
      // already running when this window opened come from the snapshot.
      try {
        const snapshot = await commands.getTranscriptionJobs();
        set((state) => ({
          jobs: snapshot
            .filter((job) => !reported.has(job.entry_id))
            .reduce(applyJob, state.jobs),
        }));
      } catch (error) {
        console.error("Failed to load transcription jobs:", error);
      } finally {
        hydrating = false;
      }
    },
  }),
);

const hasJobs = (state: TranscriptionJobsStore) =>
  Object.keys(state.jobs).length > 0;

// While a background transcription runs, the model can't be switched,
// unloaded or deleted, so one transcript is never written by two models.
export const useModelsLocked = () => useTranscriptionJobsStore(hasJobs);

// The same, read once outside React.
export const areModelsLocked = () =>
  hasJobs(useTranscriptionJobsStore.getState());
