import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Mic, Speaker, type LucideIcon } from "lucide-react";
import {
  commands,
  events,
  type MeetingRecording,
  type MeetingSourceState,
} from "@/bindings";
import i18n, { syncLanguageFromSettings } from "@/i18n";
import { getLanguageDirection } from "@/lib/utils/rtl";
import { formatClock } from "@/components/settings/meetings/format";
import "./MeetingIndicator.css";

interface MeterProps {
  icon: LucideIcon;
  label: string;
  state: MeetingSourceState;
  level: number;
}

// One side's input: a level while it records, otherwise why it doesn't.
// A side the settings leave out isn't shown.
const Meter: React.FC<MeterProps> = ({ icon: Icon, label, state, level }) => {
  const { t } = useTranslation();
  if (state === "off") return null;
  const why =
    state === "failed"
      ? t("meetings.record.sourceFailed")
      : state === "reconnecting"
        ? t("meetings.record.sourceReconnecting")
        : null;
  return (
    <span
      className={`mi-meter ${state}`}
      title={why ? `${label}: ${why}` : label}
    >
      <Icon aria-hidden />
      <span
        className="mi-bar"
        role="meter"
        aria-label={label}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.round(Math.min(1, level) * 100)}
      >
        <i
          style={{
            width: `${(state === "recording" ? Math.min(1, Math.max(0, level)) * 100 : 0).toFixed(1)}%`,
          }}
        />
      </span>
    </span>
  );
};

/** The bar shown on screen while a meeting records: time, levels and Stop. */
const MeetingIndicator: React.FC = () => {
  const { t } = useTranslation();
  const [recording, setRecording] = useState<MeetingRecording | null>(null);
  const [levels, setLevels] = useState({ microphone: 0, system_audio: 0 });
  // Elapsed time as last reported, and when, so the clock runs between
  // reports.
  const [clock, setClock] = useState({ ms: 0, at: performance.now() });
  const [now, setNow] = useState(() => performance.now());
  const [stopping, setStopping] = useState(false);

  useEffect(() => {
    let disposed = false;
    const unlisteners = [
      events.meetingRecordingChanged.listen((event) => {
        const next = event.payload.recording;
        setRecording(next);
        if (next) setClock({ ms: next.elapsed_ms, at: performance.now() });
        else setStopping(false);
      }),
      events.meetingLevels.listen((event) => {
        setLevels({
          microphone: event.payload.microphone,
          system_audio: event.payload.system_audio,
        });
        setClock({ ms: event.payload.elapsed_ms, at: performance.now() });
      }),
    ];
    void syncLanguageFromSettings().catch(console.error);
    void commands.getMeetingRecording().then((result) => {
      if (disposed || result.status !== "ok" || !result.data) return;
      setRecording(result.data);
      setClock({ ms: result.data.elapsed_ms, at: performance.now() });
    });
    return () => {
      disposed = true;
      for (const unlisten of unlisteners) void unlisten.then((off) => off());
    };
  }, []);

  useEffect(() => {
    if (!recording) return;
    const timer = window.setInterval(() => setNow(performance.now()), 500);
    return () => window.clearInterval(timer);
  }, [recording]);

  if (!recording) return null;

  const stop = async () => {
    setStopping(true);
    try {
      const result = await commands.stopMeeting();
      if (result.status !== "ok") throw new Error(result.error);
    } catch (error) {
      console.error("Failed to stop the meeting:", error);
      setStopping(false);
    }
  };

  const label = t("meetings.record.recording");
  const elapsed = formatClock(clock.ms + Math.max(0, now - clock.at));

  return (
    <div className="mi-stage" dir={getLanguageDirection(i18n.language)}>
      <div className="mi-card" role="status" aria-label={`${label} ${elapsed}`}>
        <span className="mi-dot" aria-hidden="true" />
        <span className="mi-label">{label}</span>
        <span className="mi-time">{elapsed}</span>
        <Meter
          icon={Mic}
          label={t("meetings.record.microphoneLevel")}
          state={recording.microphone}
          level={levels.microphone}
        />
        <Meter
          icon={Speaker}
          label={t("meetings.record.systemLevel")}
          state={recording.system_audio}
          level={levels.system_audio}
        />
        <button
          type="button"
          className="mi-stop"
          disabled={stopping}
          onClick={() => void stop()}
          aria-label={t("meetings.record.stop")}
          title={t("meetings.record.stop")}
        >
          <span className="mi-stop-icon" aria-hidden="true" />
        </button>
      </div>
    </div>
  );
};

export default MeetingIndicator;
