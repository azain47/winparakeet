import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { ChevronDown } from "lucide-react";
import { commands, type MeetingAudioRetention } from "@/bindings";
import { useSettings } from "@/hooks/useSettings";
import { DIARIZER_SIZE, useDiarizer } from "@/hooks/useDiarizer";
import { notify } from "@/stores/noticeStore";
import { Button } from "../../ui/Button";
import { Dialog } from "../../ui/Dialog";
import { Dropdown } from "../../ui/Dropdown";
import { SettingContainer } from "../../ui/SettingContainer";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { Switch } from "../../ui/Switch";
import { LanguageDetectorDownload } from "../LanguageDetectorDownload";
import { SAME_AS_DICTATION, useModelOptions } from "./useModelOptions";

const RETENTIONS: MeetingAudioRetention[] = [
  "keep",
  "after_transcription",
  "days_7",
  "days_30",
  "days_90",
];

// Splitting the other side into speakers; turning it on fetches the model.
const SpeakerLabels: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const enabled = getSetting("meeting_speaker_labels") ?? true;
  const diarizer = useDiarizer();
  const missing = diarizer.downloaded === false;
  // Without the model nothing is split, so it shows off unless it's on its way.
  const checked = enabled && (!missing || diarizer.downloading);
  const label = t("meetings.settings.speakerLabels.title");

  const handleChange = async (on: boolean) => {
    // Turning it off mid-download is how it's stopped.
    if (!on && diarizer.downloading) diarizer.cancel();
    await updateSetting("meeting_speaker_labels", on);
    // Turning it on is the consent to fetch the model it needs.
    if (on && missing) await diarizer.download();
  };

  return (
    <SettingContainer
      title={label}
      description={t("meetings.settings.speakerLabels.description")}
      descriptionMode="inline"
      grouped
    >
      <div className="flex items-center gap-3">
        {missing && (
          <LanguageDetectorDownload
            downloading={diarizer.downloading}
            percent={diarizer.percent}
            size={DIARIZER_SIZE}
            onCancel={diarizer.cancel}
            stopLabel={t("meetings.settings.speakerLabels.stopDownload")}
            hint={t(
              diarizer.downloading
                ? "meetings.settings.speakerLabels.downloadingHint"
                : "meetings.settings.speakerLabels.downloadHint",
            )}
          />
        )}
        <Switch
          checked={checked}
          onChange={(on) => void handleChange(on)}
          disabled={isUpdating("meeting_speaker_labels")}
          ariaLabel={label}
        />
      </div>
    </SettingContainer>
  );
};

// How long meeting audio is kept; a choice that moves audio now asks first.
const AudioRetention: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const retention = getSetting("meeting_audio_retention") ?? "keep";
  const [pending, setPending] = useState<{
    retention: MeetingAudioRetention;
    count: number;
  } | null>(null);
  const [saving, setSaving] = useState(false);

  const choose = async (value: string) => {
    const next = value as MeetingAudioRetention;
    if (next === retention) return;
    try {
      const result = await commands.countMeetingAudioToBin(next);
      if (result.status !== "ok") throw new Error(result.error);
      if (result.data > 0) {
        setPending({ retention: next, count: result.data });
        return;
      }
    } catch (error) {
      console.error("Failed to count meeting audio:", error);
      notify({ title: t("meetings.settings.retention.countFailed") });
      return;
    }
    await updateSetting("meeting_audio_retention", next);
  };

  const confirm = async () => {
    if (!pending) return;
    setSaving(true);
    try {
      await updateSetting("meeting_audio_retention", pending.retention);
    } finally {
      setSaving(false);
      setPending(null);
    }
  };

  return (
    <SettingContainer
      title={t("meetings.settings.retention.title")}
      description={t("meetings.settings.retention.description")}
      descriptionMode="inline"
      grouped
    >
      <Dropdown
        options={RETENTIONS.map((value) => ({
          value,
          label: t(`meetings.settings.retention.${value}`),
        }))}
        selectedValue={retention}
        onSelect={(value) => void choose(value)}
        disabled={isUpdating("meeting_audio_retention")}
      />
      <Dialog
        open={pending !== null}
        title={t("meetings.settings.retention.confirmTitle")}
        closeLabel={t("common.close")}
        dismissible={!saving}
        onOpenChange={(open) => {
          if (!open) setPending(null);
        }}
        footer={
          <>
            <Button
              variant="secondary"
              disabled={saving}
              onClick={() => setPending(null)}
            >
              {t("meetings.cancel")}
            </Button>
            <Button
              variant="primary"
              disabled={saving}
              onClick={() => void confirm()}
            >
              {t("meetings.settings.retention.confirm")}
            </Button>
          </>
        }
      >
        <p className="text-sm text-ink-2">
          {t("meetings.settings.retention.confirmDescription", {
            count: pending?.count ?? 0,
          })}
        </p>
      </Dialog>
    </SettingContainer>
  );
};

/** The meeting settings, folded under the list until opened. */
export const MeetingSettings: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const [open, setOpen] = useState(false);
  const sources = getSetting("meeting_sources") ?? "both";
  const meetingModel = getSetting("meeting_model") ?? null;
  const modelOptions = useModelOptions(meetingModel);

  return (
    <section className="space-y-2">
      <button
        type="button"
        onClick={() => setOpen(!open)}
        aria-expanded={open}
        className="flex w-full cursor-pointer items-center gap-2 rounded-[4px] px-1 py-1 text-start focus:outline-none focus-visible:ring-2 focus-visible:ring-logo-primary"
      >
        <span className="flex-1">
          <span className="block text-[14px] font-semibold text-text">
            {t("meetings.settings.title")}
          </span>
          <span className="mt-0.5 block text-xs text-ink-2">
            {t("meetings.settings.description")}
          </span>
        </span>
        <ChevronDown
          className={`size-4 shrink-0 text-ink-2 transition-transform ${open ? "rotate-180" : ""}`}
          aria-hidden="true"
        />
      </button>
      {open && (
        <SettingsGroup>
          <SettingContainer
            title={t("meetings.settings.sources.title")}
            description={t("meetings.settings.sources.description")}
            descriptionMode="inline"
            grouped
          >
            <Dropdown
              options={(["both", "microphone", "system"] as const).map(
                (value) => ({
                  value,
                  label: t(`meetings.settings.sources.${value}`),
                }),
              )}
              selectedValue={sources}
              onSelect={(value) =>
                void updateSetting(
                  "meeting_sources",
                  value as "both" | "microphone" | "system",
                )
              }
              disabled={isUpdating("meeting_sources")}
            />
          </SettingContainer>
          <SettingContainer
            title={t("meetings.settings.model.title")}
            description={t("meetings.settings.model.description")}
            descriptionMode="inline"
            grouped
          >
            <Dropdown
              options={modelOptions}
              selectedValue={meetingModel ?? SAME_AS_DICTATION}
              onSelect={(value) =>
                void updateSetting(
                  "meeting_model",
                  value === SAME_AS_DICTATION ? null : value,
                )
              }
              disabled={isUpdating("meeting_model")}
            />
          </SettingContainer>
          <SpeakerLabels />
          <AudioRetention />
        </SettingsGroup>
      )}
    </section>
  );
};
