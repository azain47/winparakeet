import React, { useCallback, useEffect, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { readFile } from "@tauri-apps/plugin-fs";
import { AudioLines, FileAudio, Mic, Trash2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import { notify } from "@/stores/noticeStore";
import { commands, events, type BinItem } from "@/bindings";
import { useOsType } from "@/hooks/useOsType";
import { formatDateTime } from "@/utils/dateFormat";
import { AudioPlayer, AudioPlayerGroup } from "../../ui/AudioPlayer";
import { Button } from "../../ui/Button";
import { Dialog } from "../../ui/Dialog";

// Text past this many lines is cut off; the Bin is for finding things, not
// reading them.
const CLAMPED_LINES = 3;

const itemKey = (item: BinItem) => `${item.kind}-${item.entry.id}`;

export const BinSettings: React.FC = () => {
  const { t } = useTranslation();
  const osType = useOsType();
  const [items, setItems] = useState<BinItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [confirmEmpty, setConfirmEmpty] = useState(false);
  const [pendingDelete, setPendingDelete] = useState<BinItem | null>(null);
  const [busy, setBusy] = useState(false);
  // Only the latest load may set the list; events can overlap loads.
  const loadSeq = useRef(0);

  const load = useCallback(async () => {
    const seq = ++loadSeq.current;
    try {
      const result = await commands.getBinItems();
      if (seq !== loadSeq.current) return;
      if (result.status === "ok") {
        setItems(result.data);
      } else {
        throw new Error(result.error);
      }
    } catch (error) {
      console.error("Failed to load the Bin:", error);
      if (seq === loadSeq.current) {
        notify({ title: t("settings.bin.loadFailed") });
      }
    } finally {
      if (seq === loadSeq.current) setLoading(false);
    }
  }, [t]);

  useEffect(() => {
    void load();
    const unlisten = events.binChanged.listen(() => {
      void load();
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [load]);

  const getAudioUrl = useCallback(
    async (fileName: string) => {
      try {
        const result = await commands.getAudioFilePath(fileName);
        if (result.status === "ok") {
          if (osType === "linux") {
            const fileData = await readFile(result.data);
            const blob = new Blob([fileData], { type: "audio/wav" });
            return URL.createObjectURL(blob);
          }
          return convertFileSrc(result.data, "asset");
        }
        return null;
      } catch (error) {
        console.error("Failed to get audio file path:", error);
        return null;
      }
    },
    [osType],
  );

  const restore = async (item: BinItem) => {
    try {
      const result = await commands.restoreBinItem(item.entry.id, item.kind);
      if (result.status !== "ok") throw new Error(result.error);
      setItems((prev) => prev.filter((i) => itemKey(i) !== itemKey(item)));
    } catch (error) {
      console.error("Failed to restore a Bin item:", error);
      notify({ title: t("settings.bin.restoreFailed") });
    }
  };

  const deletePermanently = async () => {
    if (!pendingDelete) return;
    const item = pendingDelete;
    setBusy(true);
    try {
      const result = await commands.deleteBinItem(item.entry.id, item.kind);
      if (result.status !== "ok") throw new Error(result.error);
      setItems((prev) => prev.filter((i) => itemKey(i) !== itemKey(item)));
    } catch (error) {
      console.error("Failed to delete a Bin item:", error);
      notify({ title: t("settings.bin.deleteFailed") });
    } finally {
      setBusy(false);
      setPendingDelete(null);
    }
  };

  const emptyBin = async () => {
    const before = items.length;
    setBusy(true);
    try {
      const result = await commands.emptyBin();
      if (result.status !== "ok") throw new Error(result.error);
      if (result.data < before) {
        notify({ title: t("settings.bin.emptyPartial") });
      }
    } catch (error) {
      console.error("Failed to empty the Bin:", error);
      notify({ title: t("settings.bin.emptyFailed") });
    } finally {
      setBusy(false);
      setConfirmEmpty(false);
      void load();
    }
  };

  let content: React.ReactNode;
  if (loading) {
    content = (
      <div className="px-4 py-3 text-center text-ink-2">
        {t("settings.bin.loading")}
      </div>
    );
  } else if (items.length === 0) {
    content = (
      <div className="px-4 py-3 text-center text-ink-2">
        {t("settings.bin.empty")}
      </div>
    );
  } else {
    content = (
      <AudioPlayerGroup>
        <div className="divide-y divide-line">
          {items.map((item) => (
            <BinRow
              key={itemKey(item)}
              item={item}
              getAudioUrl={getAudioUrl}
              onRestore={() => void restore(item)}
              onDelete={() => setPendingDelete(item)}
            />
          ))}
        </div>
      </AudioPlayerGroup>
    );
  }

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <div className="space-y-2">
        <div className="flex items-center justify-end gap-2 px-1">
          <Button
            onClick={() => setConfirmEmpty(true)}
            variant="secondary"
            size="sm"
            className="flex items-center gap-2"
            disabled={loading || items.length === 0}
            title={t("settings.bin.emptyBin")}
          >
            <Trash2 className="w-4 h-4" />
            <span>{t("settings.bin.emptyBin")}</span>
          </Button>
        </div>
        <div className="bg-card border border-line rounded-[8px] overflow-visible">
          {content}
        </div>
      </div>

      <Dialog
        open={confirmEmpty}
        title={t("settings.bin.emptyTitle")}
        closeLabel={t("common.close")}
        dismissible={!busy}
        onOpenChange={setConfirmEmpty}
        footer={
          <>
            <Button
              variant="secondary"
              disabled={busy}
              onClick={() => setConfirmEmpty(false)}
            >
              {t("settings.bin.cancel")}
            </Button>
            <Button
              variant="danger"
              disabled={busy}
              onClick={() => void emptyBin()}
            >
              {t("settings.bin.emptyBin")}
            </Button>
          </>
        }
      >
        <p className="text-sm text-ink-2">
          {t("settings.bin.emptyDescription")}
        </p>
      </Dialog>

      <Dialog
        open={pendingDelete !== null}
        title={t("settings.bin.deleteTitle")}
        closeLabel={t("common.close")}
        dismissible={!busy}
        onOpenChange={(open) => {
          if (!open) setPendingDelete(null);
        }}
        footer={
          <>
            <Button
              variant="secondary"
              disabled={busy}
              onClick={() => setPendingDelete(null)}
            >
              {t("settings.bin.cancel")}
            </Button>
            <Button
              variant="danger"
              disabled={busy}
              onClick={() => void deletePermanently()}
            >
              {t("settings.bin.deletePermanently")}
            </Button>
          </>
        }
      >
        <p className="text-sm text-ink-2">
          {t("settings.bin.deleteDescription")}
        </p>
      </Dialog>
    </div>
  );
};

interface BinRowProps {
  item: BinItem;
  getAudioUrl: (fileName: string) => Promise<string | null>;
  onRestore: () => void;
  onDelete: () => void;
}

const BinRow: React.FC<BinRowProps> = ({
  item,
  getAudioUrl,
  onRestore,
  onDelete,
}) => {
  const { t, i18n } = useTranslation();
  const { entry, kind, recording } = item;
  const text = entry.post_processed_text ?? entry.transcription_text;
  const hasText = text.trim().length > 0;

  const handleLoadAudio = useCallback(
    () => getAudioUrl(recording),
    [getAudioUrl, recording],
  );

  let Icon: React.ComponentType<{ className?: string }> = Mic;
  let label = t("settings.bin.dictation");
  if (kind === "recording") {
    Icon = AudioLines;
    label = t("settings.bin.recording");
  } else if (entry.source_name) {
    Icon = FileAudio;
    label = entry.source_name;
  }

  const created = formatDateTime(String(entry.timestamp), i18n.language);
  const deleted = formatDateTime(String(item.binned_at), i18n.language);

  return (
    <div className="px-4 py-3 pb-4 flex flex-col gap-3">
      <div className="flex justify-between items-start gap-3">
        <div className="flex flex-col gap-0.5 min-w-0">
          <span
            className="flex items-center gap-1.5 min-w-0 text-sm font-medium text-text"
            title={label}
          >
            <Icon className="w-3.5 h-3.5 shrink-0 text-ink-3" />
            <span className="truncate">{label}</span>
          </span>
          <span className="text-xs text-ink-2">
            {created}
            <span className="text-ink-3">
              {" · "}
              {t("settings.bin.deletedAt", { date: deleted })}
            </span>
          </span>
          {kind === "recording" && (
            <span className="text-xs text-ink-3">
              {t("settings.bin.recordingNote")}
            </span>
          )}
        </div>
        <div className="flex items-center gap-2 shrink-0">
          <Button variant="secondary" size="sm" onClick={onRestore}>
            {t("settings.bin.restore")}
          </Button>
          <Button variant="danger-ghost" size="sm" onClick={onDelete}>
            {t("settings.bin.deletePermanently")}
          </Button>
        </div>
      </div>

      <p
        className={`italic text-sm w-full ${
          hasText
            ? "text-text select-text cursor-text whitespace-pre-wrap break-words"
            : "text-ink-3"
        }`}
        style={
          hasText
            ? {
                display: "-webkit-box",
                WebkitLineClamp: CLAMPED_LINES,
                WebkitBoxOrient: "vertical",
                overflow: "hidden",
              }
            : undefined
        }
      >
        {hasText ? text : t("settings.bin.noText")}
      </p>

      {recording !== "" && (
        <AudioPlayer onLoadRequest={handleLoadAudio} className="w-full" />
      )}
    </div>
  );
};
