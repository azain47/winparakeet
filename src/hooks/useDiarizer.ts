import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { commands } from "@/bindings";
import { useModelStore } from "@/stores/modelStore";
import { formatModelSize } from "@/lib/utils/format";

/** Download id of the speaker-label model (`DIARIZER_ID` in Rust). */
export const DIARIZER_ID = "diarizer";

/** The size of the Sortformer file the backend fetches. */
export const DIARIZER_SIZE = formatModelSize(139);

/** The speaker-label model's download state, like the language detector's. */
export const useDiarizer = () => {
  const [downloaded, setDownloaded] = useState<boolean | null>(null);
  const [starting, setStarting] = useState(false);
  const progress = useModelStore(
    (state) => state.downloadProgress[DIARIZER_ID],
  );

  const refresh = useCallback(
    () => commands.isDiarizerDownloaded().then(setDownloaded),
    [],
  );

  useEffect(() => {
    refresh();
    const unlisten = listen<string>("model-download-complete", (event) => {
      if (event.payload === DIARIZER_ID) refresh();
    });
    return () => {
      unlisten.then((stop) => stop());
    };
  }, [refresh]);

  const download = useCallback(async () => {
    setStarting(true);
    try {
      // Failure notices come from the model store's download-failed listener.
      await commands.downloadDiarizer();
    } finally {
      setStarting(false);
    }
    await refresh();
  }, [refresh]);

  // Stops at once in the UI; the next download resumes where this one ended.
  const cancel = useCallback(() => {
    setStarting(false);
    return useModelStore.getState().cancelDownload(DIARIZER_ID);
  }, []);

  return {
    downloaded,
    downloading: starting || progress !== undefined,
    percent: Math.round(progress?.percentage ?? 0),
    download,
    cancel,
  };
};
