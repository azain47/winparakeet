import { open } from "@tauri-apps/plugin-dialog";
import { create } from "zustand";
import { notify } from "@/stores/noticeStore";
import i18n from "@/i18n";
import { commands } from "@/bindings";

// What the file picker offers. Dropped files of any type are still tried.
// No ogg/oga/opus/webm: they're mostly Opus (voice notes), which can't be
// decoded yet, though Vorbis .ogg works if dropped.
const MEDIA_EXTENSIONS = [
  "mp3",
  "m4a",
  "aac",
  "wav",
  "flac",
  "aif",
  "aiff",
  "mp4",
  "m4v",
  "mov",
];

const fileName = (path: string) => path.split(/[\\/]/).pop() || path;

export interface FileRead {
  id: number;
  name: string;
  // Files are read one at a time; the rest wait their turn.
  reading: boolean;
}

// Files being read before they're added to History, newest first. A long
// file takes a while to read, and History shows these in the meantime.
export const useFileReadsStore = create<{ reads: FileRead[] }>()(() => ({
  reads: [],
}));

let nextReadId = 0;

// Adds each file to History and queues it to be transcribed. Resolves to
// how many were added.
export const transcribeFiles = async (paths: string[]): Promise<number> => {
  const reads = paths.map((path) => ({
    id: nextReadId++,
    name: fileName(path),
    reading: false,
  }));
  // Reversed: each file lands in History above the one before it.
  useFileReadsStore.setState((state) => ({
    reads: [...[...reads].reverse(), ...state.reads],
  }));
  const removeReads = (ids: number[]) =>
    useFileReadsStore.setState((state) => ({
      reads: state.reads.filter((read) => !ids.includes(read.id)),
    }));
  let added = 0;
  try {
    for (const [index, path] of paths.entries()) {
      const readId = reads[index].id;
      useFileReadsStore.setState((state) => ({
        reads: state.reads.map((read) =>
          read.id === readId ? { ...read, reading: true } : read,
        ),
      }));
      // Its audio is read in full before it appears in History.
      const result = await commands
        .transcribeFile(path)
        .finally(() => removeReads([readId]));
      if (result.status === "ok") {
        added += 1;
      } else {
        console.error(`Failed to transcribe ${path}:`, result.error);
        // Keyed by file, so each bad file keeps its own row.
        notify({
          title: i18n.t("settings.history.fileUnsupported", {
            name: fileName(path),
          }),
          key: `file:${path}`,
        });
      }
    }
  } finally {
    // Otherwise a call that threw would leave the files after it waiting.
    removeReads(reads.map((read) => read.id));
  }
  return added;
};

// Asks for files to transcribe. Resolves to false if nothing was picked.
export const pickAndTranscribeFiles = async (): Promise<boolean> => {
  const selected = await open({
    multiple: true,
    directory: false,
    filters: [
      {
        name: i18n.t("settings.history.mediaFiles"),
        extensions: MEDIA_EXTENSIONS,
      },
    ],
  });
  if (!selected) return false;
  await transcribeFiles(Array.isArray(selected) ? selected : [selected]);
  return true;
};
