import React from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { ChevronRight, Users } from "lucide-react";
import type { Meeting } from "@/bindings";
import { useMeetingsStore } from "@/stores/meetingsStore";
import { formatDate } from "@/utils/dateFormat";
import { formatDuration, formatTime, meetingTitle } from "./format";

const CHIP =
  "inline-flex items-center rounded-[4px] px-2 py-0.5 text-xs font-medium whitespace-nowrap";

/** Where a meeting's transcription stands, when it isn't simply done. */
const StatusChip: React.FC<{ meeting: Meeting }> = ({ meeting }) => {
  const { t } = useTranslation();
  switch (meeting.status) {
    case "recording":
      return (
        <span className={`${CHIP} bg-error-wash text-error`}>
          {t("meetings.status.recording")}
        </span>
      );
    case "queued":
      return (
        <span className={`${CHIP} bg-wash text-ink-2`}>
          {t("meetings.status.queued")}
        </span>
      );
    case "transcribing":
      return (
        <span
          className={`${CHIP} bg-logo-primary/15 text-logo-primary tabular-nums`}
        >
          {meeting.progress !== null
            ? t("meetings.status.transcribingPercent", {
                percent: meeting.progress,
              })
            : t("meetings.status.transcribing")}
        </span>
      );
    case "failed":
      return (
        <span className={`${CHIP} bg-error-wash text-error`}>
          {t("meetings.status.failed")}
        </span>
      );
    case "recorded":
      return (
        <span className={`${CHIP} bg-wash text-ink-2`}>
          {t("meetings.status.notTranscribed")}
        </span>
      );
    default:
      return null;
  }
};

// "Today", "Yesterday", or the date, for the heading over a day's meetings.
const dayLabel = (startedAt: number, locale: string, t: TFunction) => {
  const day = new Date(startedAt * 1000);
  const today = new Date();
  const yesterday = new Date();
  yesterday.setDate(today.getDate() - 1);
  if (day.toDateString() === today.toDateString())
    return t("meetings.list.today");
  if (day.toDateString() === yesterday.toDateString())
    return t("meetings.list.yesterday");
  return formatDate(String(startedAt), locale);
};

interface MeetingListProps {
  onOpen: (id: number) => void;
}

/** Every meeting outside the Bin by day, newest first. */
export const MeetingList: React.FC<MeetingListProps> = ({ onOpen }) => {
  const { t, i18n } = useTranslation();
  const meetings = useMeetingsStore((state) => state.meetings);
  const loaded = useMeetingsStore((state) => state.loaded);

  if (!loaded || meetings.length === 0) {
    return (
      <div className="rounded-[8px] border border-line bg-card px-4 py-6 text-center text-sm text-ink-2">
        {loaded ? t("meetings.list.empty") : t("meetings.list.loading")}
      </div>
    );
  }

  const groups: { label: string; meetings: Meeting[] }[] = [];
  for (const meeting of meetings) {
    const label = dayLabel(meeting.started_at, i18n.language, t);
    const last = groups[groups.length - 1];
    if (last?.label === label) last.meetings.push(meeting);
    else groups.push({ label, meetings: [meeting] });
  }

  return (
    <div className="space-y-4">
      {groups.map((group) => (
        <section key={group.label} className="space-y-2">
          <h2 className="px-1 text-[14px] font-semibold text-text">
            {group.label}
          </h2>
          <div className="flex flex-col gap-[3px]">
            {group.meetings.map((meeting) => (
              <button
                key={meeting.id}
                type="button"
                onClick={() => onOpen(meeting.id)}
                className="group flex min-h-[60px] w-full cursor-pointer items-center gap-3 rounded-[6px] border border-line bg-card px-4 py-2.5 text-start transition-colors duration-100 hover:bg-card-hover focus:outline-none focus-visible:ring-2 focus-visible:ring-logo-primary"
              >
                <div className="min-w-0 flex-1">
                  <p className="truncate text-[14px] text-text">
                    {meetingTitle(meeting, t, i18n.language)}
                  </p>
                  <p className="mt-0.5 text-xs text-ink-2 tabular-nums">
                    {formatTime(meeting.started_at, i18n.language)}
                    {meeting.status !== "recording" && (
                      <>
                        {" · "}
                        {formatDuration(meeting.duration_ms, t)}
                      </>
                    )}
                  </p>
                </div>
                {meeting.recovered && meeting.status !== "recording" && (
                  <span className={`${CHIP} bg-warning-wash text-warning`}>
                    {t("meetings.status.recovered")}
                  </span>
                )}
                <StatusChip meeting={meeting} />
                {meeting.speakers.length > 0 && (
                  <span className="flex items-center gap-1 text-xs text-ink-3 whitespace-nowrap">
                    <Users className="size-3.5" aria-hidden="true" />
                    {t("meetings.list.speakers", {
                      count: meeting.speakers.length,
                    })}
                  </span>
                )}
                <ChevronRight
                  className="size-4 shrink-0 text-ink-3 rtl:rotate-180"
                  aria-hidden="true"
                />
              </button>
            ))}
          </div>
        </section>
      ))}
    </div>
  );
};
