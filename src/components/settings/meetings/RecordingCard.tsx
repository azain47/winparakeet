import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Mic, Speaker, Square, Users } from "lucide-react";
import { commands, type MeetingSourceState } from "@/bindings";
import { useSettings } from "@/hooks/useSettings";
import { useMeetingsStore } from "@/stores/meetingsStore";
import { notify } from "@/stores/noticeStore";
import { Button } from "../../ui/Button";
import { formatClock } from "./format";

interface LevelMeterProps {
  label: string;
  icon: React.ComponentType<{ className?: string }>;
  state: MeetingSourceState;
  level: number;
}

// One side's input: a live meter while it records, otherwise why it doesn't.
const LevelMeter: React.FC<LevelMeterProps> = ({
  label,
  icon: Icon,
  state,
  level,
}) => {
  const { t } = useTranslation();
  let status: string | null = null;
  if (state === "off") status = t("meetings.record.sourceOff");
  else if (state === "reconnecting")
    status = t("meetings.record.sourceReconnecting");
  else if (state === "failed") status = t("meetings.record.sourceFailed");

  return (
    <div className="flex items-center gap-3 min-w-0">
      <span className="flex w-24 shrink-0 items-center gap-1.5 text-sm text-ink-2">
        <Icon className="size-4 shrink-0 text-ink-3" />
        <span className="truncate">{label}</span>
      </span>
      {status ? (
        <span
          className={`text-xs ${state === "failed" ? "text-error" : state === "reconnecting" ? "text-warning" : "text-ink-3"}`}
        >
          {status}
        </span>
      ) : (
        <span
          role="meter"
          aria-label={label}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={Math.round(Math.min(1, level) * 100)}
          className="block h-[6px] flex-1 overflow-hidden rounded-full bg-line"
        >
          <i
            className="block h-full rounded-[inherit] bg-background-ui transition-[width] duration-100 ease-linear"
            style={{
              width: `${(Math.min(1, Math.max(0, level)) * 100).toFixed(1)}%`,
            }}
          />
        </span>
      )}
    </div>
  );
};

/** Starts a meeting; while one records, its time, both sides' levels and Stop. */
export const RecordingCard: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting } = useSettings();
  const recording = useMeetingsStore((state) => state.recording);
  const levels = useMeetingsStore((state) => state.levels);
  const elapsed = useMeetingsStore((state) => state.elapsed);
  const [busy, setBusy] = useState(false);
  const [now, setNow] = useState(() => performance.now());
  const sources = getSetting("meeting_sources") ?? "both";

  // The backend reports the time about ten times a second; this keeps the
  // clock moving between reports.
  useEffect(() => {
    if (!recording) return;
    const timer = window.setInterval(() => setNow(performance.now()), 250);
    return () => window.clearInterval(timer);
  }, [recording]);

  const start = async () => {
    setBusy(true);
    try {
      const result = await commands.startMeeting();
      if (result.status !== "ok") {
        notify({
          title: t("meetings.record.startFailed"),
          detail: result.error,
        });
      }
    } catch (error) {
      console.error("Failed to start a meeting:", error);
      notify({ title: t("meetings.record.startFailed") });
    } finally {
      setBusy(false);
    }
  };

  const stop = async () => {
    setBusy(true);
    try {
      const result = await commands.stopMeeting();
      if (result.status !== "ok") {
        notify({
          title: t("meetings.record.stopFailed"),
          detail: result.error,
        });
      }
    } catch (error) {
      console.error("Failed to stop the meeting:", error);
      notify({ title: t("meetings.record.stopFailed") });
    } finally {
      setBusy(false);
    }
  };

  if (!recording) {
    return (
      <div className="flex items-center gap-4 rounded-[8px] border border-line bg-card px-4 py-4">
        <span className="grid size-10 shrink-0 place-items-center rounded-full bg-logo-primary/15 text-logo-primary">
          <Users className="size-5" />
        </span>
        <div className="min-w-0 flex-1">
          <h2 className="text-[14px] font-semibold text-text">
            {t("meetings.record.title")}
          </h2>
          <p className="mt-0.5 text-xs text-ink-2">
            {t(`meetings.record.sources.${sources}`)}
          </p>
        </div>
        <Button
          variant="primary"
          disabled={busy}
          onClick={() => void start()}
          className="flex shrink-0 items-center gap-2"
        >
          <span
            className="size-2.5 rounded-full bg-current"
            aria-hidden="true"
          />
          {t("meetings.record.start")}
        </Button>
      </div>
    );
  }

  const elapsedMs = elapsed.ms + Math.max(0, now - elapsed.at);

  return (
    <div className="flex flex-col gap-3 rounded-[8px] border border-line bg-card px-4 py-4">
      <div className="flex items-center gap-3">
        <span className="relative flex size-3 shrink-0" aria-hidden="true">
          <span className="absolute inline-flex size-full animate-ping rounded-full bg-error opacity-60 motion-reduce:animate-none" />
          <span className="relative inline-flex size-3 rounded-full bg-error" />
        </span>
        <h2 className="text-[14px] font-semibold text-text">
          {t("meetings.record.recording")}
        </h2>
        <span
          role="timer"
          className="font-display text-[20px] font-semibold tabular-nums text-text"
        >
          {formatClock(elapsedMs)}
        </span>
        <Button
          variant="danger"
          disabled={busy}
          onClick={() => void stop()}
          className="ms-auto flex shrink-0 items-center gap-2"
        >
          <Square className="size-3.5" fill="currentColor" />
          {t("meetings.record.stop")}
        </Button>
      </div>
      <div className="flex flex-col gap-2">
        <LevelMeter
          label={t("meetings.speakers.me")}
          icon={Mic}
          state={recording.microphone}
          level={levels.microphone}
        />
        <LevelMeter
          label={t("meetings.speakers.them")}
          icon={Speaker}
          state={recording.system_audio}
          level={levels.system_audio}
        />
      </div>
    </div>
  );
};
