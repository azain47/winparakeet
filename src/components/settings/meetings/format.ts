import type { TFunction } from "i18next";
import type { Meeting, MeetingSpeaker } from "@/bindings";
import { formatDate } from "@/utils/dateFormat";

/** A transcript timestamp: m:ss, or h:mm:ss from an hour on. */
export const formatClock = (ms: number): string => {
  const total = Math.max(0, Math.floor(ms / 1000));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = String(total % 60).padStart(2, "0");
  return hours > 0
    ? `${hours}:${String(minutes).padStart(2, "0")}:${seconds}`
    : `${minutes}:${seconds}`;
};

/** A meeting's or speaker's length in words: "1 h 5 min", "12 min", "40 s". */
export const formatDuration = (ms: number, t: TFunction): string => {
  const total = Math.max(0, Math.round(ms / 1000));
  if (total < 60) return t("meetings.duration.seconds", { seconds: total });
  const minutes = Math.round(total / 60);
  if (minutes < 60) return t("meetings.duration.minutes", { minutes });
  return t("meetings.duration.hours", {
    hours: Math.floor(minutes / 60),
    minutes: minutes % 60,
  });
};

/** The time of day a meeting started. */
export const formatTime = (startedAt: number, locale: string): string =>
  new Intl.DateTimeFormat(locale, {
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(startedAt * 1000));

/** Its title, or "Meeting on <date>" until the user names it. */
export const meetingTitle = (
  meeting: Meeting,
  t: TFunction,
  locale: string,
): string =>
  meeting.title.trim() ||
  t("meetings.defaultTitle", {
    date: formatDate(String(meeting.started_at), locale),
  });

/** The label a speaker key shows until the user names it: Me, Them, Speaker N. */
export const defaultSpeakerName = (key: string, t: TFunction): string => {
  if (key === "me") return t("meetings.speakers.me");
  if (key === "them") return t("meetings.speakers.them");
  const number = Number(key.replace(/^s/, ""));
  return t("meetings.speakers.numbered", {
    number: Number.isFinite(number) ? number : key,
  });
};

/** The user's name for a speaker, or its default label. */
export const speakerName = (
  key: string,
  speakers: MeetingSpeaker[],
  t: TFunction,
): string =>
  speakers.find((s) => s.key === key)?.name?.trim() ||
  defaultSpeakerName(key, t);

/** Each speaker's color token (theme.css), so a name reads the same everywhere. */
export const speakerColor = (key: string): string => {
  if (key === "me") return "var(--color-speaker-me)";
  if (key === "them") return "var(--color-speaker-them)";
  const number = Number(key.replace(/^s/, ""));
  const slot =
    Number.isFinite(number) && number >= 1 ? ((number - 1) % 4) + 1 : 1;
  return `var(--color-speaker-${slot})`;
};
