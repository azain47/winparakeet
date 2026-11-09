import React, { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { convertFileSrc } from "@tauri-apps/api/core";
import { join } from "@tauri-apps/api/path";
import { save } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import {
  ArrowLeft,
  AudioLines,
  ChevronDown,
  Copy,
  FileDown,
  FolderOpen,
  RotateCcw,
  Trash2,
} from "lucide-react";
import {
  commands,
  type Meeting,
  type MeetingSegment,
  type MeetingTranscriptFormat,
} from "@/bindings";
import { usePressAway } from "@/hooks/usePressAway";
import { useMeetingsStore } from "@/stores/meetingsStore";
import { useModelStore } from "@/stores/modelStore";
import { notify } from "@/stores/noticeStore";
import { getTranslatedModelName } from "@/lib/utils/modelTranslation";
import { formatDateTime } from "@/utils/dateFormat";
import {
  AudioPlayer,
  AudioPlayerGroup,
  type AudioPlayerControl,
} from "../../ui/AudioPlayer";
import { Button } from "../../ui/Button";
import { ProgressLine } from "../../shared/ProgressLine";
import { copyToClipboard } from "../history/clipboard";
import {
  defaultSpeakerName,
  formatClock,
  formatDuration,
  meetingTitle,
  speakerColor,
  speakerName,
} from "./format";
import { RetranscribeDialog } from "./RetranscribeDialog";

const EXPORT_FORMATS: { format: MeetingTranscriptFormat; ext: string }[] = [
  { format: "txt", ext: "txt" },
  { format: "md", ext: "md" },
  { format: "srt", ext: "srt" },
  { format: "vtt", ext: "vtt" },
  { format: "json", ext: "json" },
];

// Characters Windows doesn't allow in a file name.
const fileSafe = (name: string) =>
  name.replace(/[\\/:*?"<>|]+/g, "-").trim() || "meeting";

const errorText = (error: unknown) =>
  error instanceof Error ? error.message : String(error);

interface ToolbarButtonProps {
  onClick: () => void;
  label: string;
  icon: React.ComponentType<{ className?: string }>;
  disabled?: boolean;
  title?: string;
  variant?: "secondary" | "danger-ghost";
}

const ToolbarButton: React.FC<ToolbarButtonProps> = ({
  onClick,
  label,
  icon: Icon,
  disabled,
  title,
  variant = "secondary",
}) => (
  <Button
    variant={variant}
    size="sm"
    onClick={onClick}
    disabled={disabled}
    title={title ?? label}
    className="flex items-center gap-1.5"
  >
    <Icon className="size-3.5" />
    <span>{label}</span>
  </Button>
);

// A status message across the top of the detail, with its action.
const Banner: React.FC<{
  tone: "info" | "warning" | "error";
  children: React.ReactNode;
  action?: React.ReactNode;
}> = ({ tone, children, action }) => {
  const toneClasses = {
    info: "border-line bg-card",
    warning: "border-warning/40 bg-warning-wash",
    error: "border-error/40 bg-error-wash",
  };
  return (
    <div
      className={`flex items-center gap-3 rounded-[6px] border px-4 py-3 text-sm ${toneClasses[tone]}`}
    >
      <div className="min-w-0 flex-1">{children}</div>
      {action && (
        <div className="flex shrink-0 items-center gap-2">{action}</div>
      )}
    </div>
  );
};

/** Exports the transcript in one of the formats, each through its own save dialog. */
const ExportMenu: React.FC<{
  disabled: boolean;
  onExport: (format: MeetingTranscriptFormat, ext: string) => void;
}> = ({ disabled, onExport }) => {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  usePressAway(open, ref, () => setOpen(false));

  return (
    <div
      ref={ref}
      className="relative"
      onKeyDown={(event) => {
        if (event.key === "Escape" && open) {
          event.stopPropagation();
          setOpen(false);
        }
      }}
    >
      <Button
        variant="secondary"
        size="sm"
        onClick={() => setOpen(!open)}
        disabled={disabled}
        aria-expanded={open}
        aria-haspopup="menu"
        className="flex items-center gap-1.5"
      >
        <FileDown className="size-3.5" />
        <span>{t("meetings.detail.exportTranscript")}</span>
        <ChevronDown className="size-3" />
      </Button>
      {open && (
        <div
          role="menu"
          className="absolute start-0 top-full z-50 mt-1 min-w-48 animate-drop rounded-[8px] border border-line bg-flyout p-1 shadow-flyout"
        >
          {EXPORT_FORMATS.map(({ format, ext }) => (
            <button
              key={format}
              type="button"
              role="menuitem"
              onClick={() => {
                setOpen(false);
                onExport(format, ext);
              }}
              className="flex w-full cursor-pointer items-center justify-between gap-4 rounded-[4px] px-3 py-1.5 text-start text-sm hover:bg-wash focus:outline-none focus-visible:bg-wash"
            >
              <span>{t(`meetings.export.${format}`)}</span>
              <span className="text-xs text-ink-3">.{ext}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
};

/** A speaker's name, colored like their lines; click it to rename them. */
const SpeakerChip: React.FC<{
  meeting: Meeting;
  speakerKey: string;
  talkMs: number;
}> = ({ meeting, speakerKey, talkMs }) => {
  const { t } = useTranslation();
  const [editing, setEditing] = useState(false);
  const name = speakerName(speakerKey, meeting.speakers, t);
  const [draft, setDraft] = useState(name);

  const commit = async () => {
    setEditing(false);
    const trimmed = draft.trim();
    const current =
      meeting.speakers.find((s) => s.key === speakerKey)?.name ?? null;
    // Empty goes back to the default label.
    const next = trimmed === "" ? null : trimmed;
    if (next === current) return;
    try {
      const result = await commands.renameMeetingSpeaker(
        meeting.id,
        speakerKey,
        next,
      );
      if (result.status !== "ok") throw new Error(result.error);
    } catch (error) {
      console.error("Failed to rename a speaker:", error);
      notify({ title: t("meetings.detail.renameFailed") });
    }
  };

  return (
    <div className="flex items-center gap-2 rounded-[6px] border border-line bg-card px-3 py-1.5">
      <span
        aria-hidden="true"
        className="size-2.5 shrink-0 rounded-full"
        style={{ background: speakerColor(speakerKey) }}
      />
      {editing ? (
        <input
          autoFocus
          value={draft}
          placeholder={defaultSpeakerName(speakerKey, t)}
          aria-label={t("meetings.detail.renameSpeaker")}
          onChange={(event) => setDraft(event.target.value)}
          onBlur={() => void commit()}
          onKeyDown={(event) => {
            if (event.key === "Enter") event.currentTarget.blur();
            if (event.key === "Escape") {
              event.stopPropagation();
              setDraft(name);
              setEditing(false);
            }
          }}
          className="w-32 rounded-[4px] border border-line bg-wash px-1.5 py-0.5 text-sm focus:border-b-logo-primary focus:bg-card focus:outline-none"
        />
      ) : (
        <button
          type="button"
          title={t("meetings.detail.renameSpeaker")}
          onClick={() => {
            setDraft(name);
            setEditing(true);
          }}
          className="cursor-text text-sm font-medium hover:underline focus:outline-none focus-visible:ring-1 focus-visible:ring-logo-primary"
          style={{ color: speakerColor(speakerKey) }}
        >
          {name}
        </button>
      )}
      <span className="text-xs text-ink-3 tabular-nums">
        {formatDuration(talkMs, t)}
      </span>
    </div>
  );
};

interface MeetingDetailProps {
  meetingId: number;
  onBack: () => void;
  onOpenModels: () => void;
}

/** One meeting: its status, speakers, audio, transcript and what can be done with them. */
export const MeetingDetail: React.FC<MeetingDetailProps> = ({
  meetingId,
  onBack,
  onOpenModels,
}) => {
  const { t, i18n } = useTranslation();
  const meeting = useMeetingsStore((state) =>
    state.meetings.find((m) => m.id === meetingId),
  );
  const loaded = useMeetingsStore((state) => state.loaded);
  const models = useModelStore((state) => state.models);
  const [segments, setSegments] = useState<MeetingSegment[] | null>(null);
  const [titleDraft, setTitleDraft] = useState(meeting?.title ?? "");
  const [retranscribeOpen, setRetranscribeOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [copied, setCopied] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const playerRef = useRef<AudioPlayerControl | null>(null);

  // Opening a meeting starts at the top of the page.
  useEffect(() => {
    rootRef.current?.closest(".overflow-y-auto")?.scrollTo({ top: 0 });
  }, [meetingId]);

  // Gone to the Bin (here or from elsewhere): back to the list.
  useEffect(() => {
    if (loaded && !meeting) onBack();
  }, [loaded, meeting, onBack]);

  useEffect(() => {
    setTitleDraft(meeting?.title ?? "");
  }, [meeting?.title]);

  // The transcript changes only when a transcription ends, which changes the
  // status or the model; progress updates don't need it again.
  const status = meeting?.status;
  const modelId = meeting?.model_id;
  useEffect(() => {
    let cancelled = false;
    commands
      .getMeeting(meetingId)
      .then((result) => {
        if (cancelled) return;
        if (result.status === "ok") setSegments(result.data.segments);
        else throw new Error(result.error);
      })
      .catch((error) => {
        console.error("Failed to load the meeting:", error);
        if (!cancelled) setSegments([]);
      });
    return () => {
      cancelled = true;
    };
  }, [meetingId, status, modelId]);

  const handleLoadAudio = useCallback(async () => {
    try {
      const result = await commands.getMeetingAudioPath(meetingId);
      if (result.status !== "ok") throw new Error(result.error);
      return convertFileSrc(result.data, "asset");
    } catch (error) {
      console.error("Failed to get the meeting's audio:", error);
      notify({ title: t("meetings.detail.audioFailed") });
      return null;
    }
  }, [meetingId, t]);

  if (!meeting) {
    return <div ref={rootRef} />;
  }

  const title = meetingTitle(meeting, t, i18n.language);
  const audioKept = meeting.audio === "kept";
  const working =
    meeting.status === "recording" ||
    meeting.status === "queued" ||
    meeting.status === "transcribing";
  const hasTranscript = (segments?.length ?? 0) > 0;
  const model = models.find((m) => m.id === meeting.model_id);
  const modelName = model
    ? getTranslatedModelName(model, t)
    : (meeting.model_id ?? null);
  let languageName: string | null = null;
  if (meeting.language) {
    try {
      languageName =
        new Intl.DisplayNames([i18n.language], { type: "language" }).of(
          meeting.language,
        ) ?? meeting.language;
    } catch {
      languageName = meeting.language;
    }
  }
  const meta = [
    formatDateTime(String(meeting.started_at), i18n.language),
    meeting.status !== "recording"
      ? formatDuration(meeting.duration_ms, t)
      : null,
    modelName,
    languageName,
  ].filter(Boolean);

  // Every speaker key in the transcript, named as the page shows it.
  const speakerNames: Record<string, string> = {};
  for (const key of [
    ...meeting.speakers.map((s) => s.key),
    ...(segments ?? []).map((s) => s.speaker),
  ]) {
    speakerNames[key] = speakerName(key, meeting.speakers, t);
  }

  const plainTranscript = () =>
    (segments ?? [])
      .map(
        (s) =>
          `[${formatClock(s.start_ms)}] ${speakerNames[s.speaker]}: ${s.text}`,
      )
      .join("\n");

  const saveTitle = async () => {
    const next = titleDraft.trim();
    if (next === meeting.title.trim()) {
      setTitleDraft(meeting.title);
      return;
    }
    try {
      const result = await commands.renameMeeting(meeting.id, next);
      if (result.status !== "ok") throw new Error(result.error);
    } catch (error) {
      console.error("Failed to rename the meeting:", error);
      setTitleDraft(meeting.title);
      notify({ title: t("meetings.detail.renameFailed") });
    }
  };

  const run = async (
    action: () => Promise<
      { status: "ok" } | { status: "error"; error: string }
    >,
    failure: string,
  ) => {
    setBusy(true);
    try {
      const result = await action();
      if (result.status !== "ok") throw new Error(result.error);
      return true;
    } catch (error) {
      console.error(`${failure}:`, error);
      notify({ title: t(failure), detail: errorText(error) });
      return false;
    } finally {
      setBusy(false);
    }
  };

  const transcribe = () =>
    void run(
      () => commands.transcribeMeeting(meeting.id, null),
      "meetings.detail.transcribeFailed",
    );

  const cancelTranscription = () =>
    void run(
      () => commands.cancelMeetingTranscription(meeting.id),
      "meetings.detail.cancelFailed",
    );

  const copyTranscript = async () => {
    if (!(await copyToClipboard(plainTranscript()))) {
      notify({ title: t("meetings.detail.copyFailed") });
      return;
    }
    setCopied(true);
    window.setTimeout(() => setCopied(false), 2000);
  };

  const exportTranscript = async (
    format: MeetingTranscriptFormat,
    ext: string,
  ) => {
    try {
      const path = await save({
        defaultPath: `${fileSafe(title)}.${ext}`,
        filters: [{ name: t(`meetings.export.${format}`), extensions: [ext] }],
      });
      if (!path) return;
      const result = await commands.exportMeetingTranscript(
        meeting.id,
        format,
        path,
        speakerNames,
      );
      if (result.status !== "ok") throw new Error(result.error);
    } catch (error) {
      console.error("Failed to export the transcript:", error);
      notify({
        title: t("meetings.detail.exportFailed"),
        detail: errorText(error),
      });
    }
  };

  const saveAudio = async () => {
    try {
      const path = await save({
        defaultPath: `${fileSafe(title)}.wav`,
        filters: [{ name: t("meetings.export.wav"), extensions: ["wav"] }],
      });
      if (!path) return;
      setBusy(true);
      const result = await commands.exportMeetingAudio(meeting.id, path);
      if (result.status !== "ok") throw new Error(result.error);
    } catch (error) {
      console.error("Failed to save the meeting's audio:", error);
      notify({
        title: t("meetings.detail.saveAudioFailed"),
        detail: errorText(error),
      });
    } finally {
      setBusy(false);
    }
  };

  const openFolder = async () => {
    try {
      const result = await commands.getMeetingFolder(meeting.id);
      if (result.status !== "ok") throw new Error(result.error);
      // Revealing a track opens the folder itself; without one, its parent
      // opens with the folder picked.
      const track = !audioKept
        ? null
        : meeting.has_microphone
          ? "microphone.wav"
          : meeting.has_system_audio
            ? "system.wav"
            : null;
      await revealItemInDir(
        track ? await join(result.data, track) : result.data,
      );
    } catch (error) {
      console.error("Failed to open the meeting's folder:", error);
      notify({ title: t("meetings.detail.openFolderFailed") });
    }
  };

  const restore = async (kind: "meeting" | "meeting_audio", id: number) => {
    try {
      const result = await commands.restoreBinItem(id, kind);
      if (result.status !== "ok") throw new Error(result.error);
    } catch (error) {
      console.error("Failed to restore from the Bin:", error);
      notify({ title: t("meetings.detail.undoFailed") });
    }
  };

  const binAudio = async () => {
    const id = meeting.id;
    if (
      await run(
        () => commands.binMeetingAudio(id),
        "meetings.detail.binAudioFailed",
      )
    ) {
      notify({
        title: t("meetings.detail.audioMovedToBin"),
        level: "warning",
        action: {
          label: t("meetings.detail.undo"),
          onClick: () => void restore("meeting_audio", id),
        },
      });
    }
  };

  const binMeeting = async () => {
    const id = meeting.id;
    if (await run(() => commands.binMeeting(id), "meetings.detail.binFailed")) {
      onBack();
      notify({
        title: t("meetings.detail.movedToBin"),
        level: "warning",
        action: {
          label: t("meetings.detail.undo"),
          onClick: () => void restore("meeting", id),
        },
      });
    }
  };

  let banner: React.ReactNode = null;
  if (meeting.status === "recording") {
    banner = <Banner tone="info">{t("meetings.detail.recordingNow")}</Banner>;
  } else if (meeting.status === "transcribing") {
    const percent = meeting.progress ?? 0;
    banner = (
      <Banner
        tone="info"
        action={
          <Button
            variant="secondary"
            size="sm"
            disabled={busy}
            onClick={cancelTranscription}
          >
            {t("meetings.cancel")}
          </Button>
        }
      >
        <p className="mb-2 tabular-nums">
          {meeting.progress !== null
            ? t("meetings.status.transcribingPercent", { percent })
            : t("meetings.status.transcribing")}
        </p>
        <ProgressLine
          value={percent / 100}
          busy={meeting.progress === null}
          className="h-[4px]"
        />
      </Banner>
    );
  } else if (meeting.status === "queued") {
    banner = (
      <Banner
        tone="info"
        action={
          <Button
            variant="secondary"
            size="sm"
            disabled={busy}
            onClick={cancelTranscription}
          >
            {t("meetings.cancel")}
          </Button>
        }
      >
        {t("meetings.detail.queued")}
      </Banner>
    );
  } else if (meeting.status === "failed") {
    banner = (
      <Banner
        tone="error"
        action={
          audioKept && (
            <Button
              variant="secondary"
              size="sm"
              disabled={busy}
              onClick={transcribe}
            >
              {t("meetings.detail.retry")}
            </Button>
          )
        }
      >
        <p className="font-medium text-error">{t("meetings.detail.failed")}</p>
        {meeting.error && (
          <p className="mt-0.5 text-xs text-ink-2 select-text break-words">
            {meeting.error}
          </p>
        )}
      </Banner>
    );
  } else if (meeting.status === "recorded") {
    banner = (
      <Banner
        tone="info"
        action={
          audioKept && (
            <Button
              variant="primary"
              size="sm"
              disabled={busy}
              onClick={transcribe}
            >
              {t("meetings.detail.transcribe")}
            </Button>
          )
        }
      >
        {t("meetings.detail.notTranscribed")}
      </Banner>
    );
  }

  return (
    <div ref={rootRef} className="space-y-4">
      <div className="flex items-start gap-2">
        <button
          type="button"
          onClick={onBack}
          title={t("meetings.detail.back")}
          aria-label={t("meetings.detail.back")}
          className="mt-0.5 grid size-8 shrink-0 cursor-pointer place-items-center rounded-[4px] text-ink-2 hover:bg-wash hover:text-text focus:outline-none focus-visible:ring-2 focus-visible:ring-logo-primary"
        >
          <ArrowLeft className="size-4 rtl:rotate-180" />
        </button>
        <div className="min-w-0 flex-1">
          <input
            value={titleDraft}
            placeholder={meetingTitle(
              { ...meeting, title: "" },
              t,
              i18n.language,
            )}
            aria-label={t("meetings.detail.title")}
            onChange={(event) => setTitleDraft(event.target.value)}
            onBlur={() => void saveTitle()}
            onKeyDown={(event) => {
              if (event.key === "Enter") event.currentTarget.blur();
              if (event.key === "Escape") {
                setTitleDraft(meeting.title);
                event.currentTarget.blur();
              }
            }}
            className="w-full rounded-[4px] border border-transparent bg-transparent px-1.5 py-0.5 font-display text-[20px] font-semibold text-text placeholder:text-text hover:border-line focus:border-line focus:border-b-logo-primary focus:bg-card focus:outline-none focus:placeholder:text-ink-3"
          />
          <p className="px-1.5 text-xs text-ink-2">{meta.join(" · ")}</p>
        </div>
      </div>

      {banner}

      {meeting.recovered && (
        <Banner tone="warning">{t("meetings.detail.recovered")}</Banner>
      )}

      {meeting.audio === "binned" && (
        <Banner
          tone="info"
          action={
            <Button
              variant="secondary"
              size="sm"
              onClick={() => void restore("meeting_audio", meeting.id)}
            >
              {t("meetings.detail.restoreAudio")}
            </Button>
          }
        >
          {t("meetings.detail.audioBinned")}
        </Banner>
      )}
      {meeting.audio === "deleted" && (
        <Banner tone="info">{t("meetings.detail.audioDeleted")}</Banner>
      )}

      {meeting.status !== "recording" && (
        <div className="flex flex-wrap items-center gap-2">
          <ToolbarButton
            icon={RotateCcw}
            label={t("meetings.detail.retranscribe")}
            disabled={!audioKept || working || busy}
            onClick={() => setRetranscribeOpen(true)}
          />
          <ToolbarButton
            icon={Copy}
            label={
              copied ? t("meetings.detail.copied") : t("meetings.detail.copy")
            }
            disabled={!hasTranscript}
            onClick={() => void copyTranscript()}
          />
          <ExportMenu
            disabled={!hasTranscript}
            onExport={(format, ext) => void exportTranscript(format, ext)}
          />
          <ToolbarButton
            icon={AudioLines}
            label={t("meetings.detail.saveAudio")}
            disabled={!audioKept || busy}
            onClick={() => void saveAudio()}
          />
          <ToolbarButton
            icon={FolderOpen}
            label={t("meetings.detail.openFolder")}
            onClick={() => void openFolder()}
          />
          <div className="ms-auto flex items-center gap-2">
            <ToolbarButton
              icon={Trash2}
              variant="danger-ghost"
              label={t("meetings.detail.binAudio")}
              disabled={!audioKept || working || busy}
              onClick={() => void binAudio()}
            />
            <ToolbarButton
              icon={Trash2}
              variant="danger-ghost"
              label={t("meetings.detail.binMeeting")}
              disabled={busy}
              onClick={() => void binMeeting()}
            />
          </div>
        </div>
      )}

      {meeting.speakers.length > 0 && (
        <section className="space-y-2">
          <h2 className="px-1 text-[14px] font-semibold text-text">
            {t("meetings.detail.speakers")}
          </h2>
          <div className="flex flex-wrap gap-2">
            {meeting.speakers.map((speaker) => (
              <SpeakerChip
                key={speaker.key}
                meeting={meeting}
                speakerKey={speaker.key}
                talkMs={speaker.talk_ms}
              />
            ))}
          </div>
        </section>
      )}

      {audioKept && meeting.status !== "recording" && (
        <AudioPlayerGroup>
          <div className="rounded-[6px] border border-line bg-card px-4 py-3">
            <AudioPlayer
              onLoadRequest={handleLoadAudio}
              controlRef={playerRef}
              className="w-full"
            />
          </div>
        </AudioPlayerGroup>
      )}

      {segments !== null && (meeting.status === "ready" || hasTranscript) && (
        <section className="space-y-2">
          <h2 className="px-1 text-[14px] font-semibold text-text">
            {t("meetings.detail.transcript")}
          </h2>
          <div className="rounded-[8px] border border-line bg-card">
            {hasTranscript ? (
              <div className="divide-y divide-line">
                {segments.map((segment, index) => (
                  <div
                    key={`${segment.start_ms}-${index}`}
                    className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-0.5 px-4 py-2.5"
                  >
                    <button
                      type="button"
                      disabled={!audioKept}
                      onClick={() =>
                        playerRef.current?.seek(segment.start_ms / 1000)
                      }
                      title={
                        audioKept ? t("meetings.detail.playFrom") : undefined
                      }
                      className="row-span-2 min-w-12 cursor-pointer self-start pt-px text-start text-xs text-ink-3 tabular-nums hover:text-logo-primary disabled:cursor-default disabled:hover:text-ink-3 focus:outline-none focus-visible:ring-1 focus-visible:ring-logo-primary"
                    >
                      {formatClock(segment.start_ms)}
                    </button>
                    <span
                      className="text-xs font-semibold"
                      style={{ color: speakerColor(segment.speaker) }}
                    >
                      {speakerNames[segment.speaker]}
                    </span>
                    <p className="text-sm text-text select-text cursor-text whitespace-pre-wrap break-words">
                      {segment.text}
                    </p>
                  </div>
                ))}
              </div>
            ) : (
              <p className="px-4 py-3 text-center text-sm text-ink-2">
                {t("meetings.detail.noSpeech")}
              </p>
            )}
          </div>
        </section>
      )}

      <RetranscribeDialog
        meetingId={meeting.id}
        open={retranscribeOpen}
        onOpenChange={setRetranscribeOpen}
        onOpenModels={onOpenModels}
      />
    </div>
  );
};
