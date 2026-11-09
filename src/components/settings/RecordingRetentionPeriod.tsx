import React, { useState } from "react";
import { Trans, useTranslation } from "react-i18next";
import { Button } from "../ui/Button";
import { Dialog } from "../ui/Dialog";
import { Dropdown } from "../ui/Dropdown";
import { Input } from "../ui/Input";
import { Radio } from "../ui/Radio";
import { SettingContainer } from "../ui/SettingContainer";
import { useSettings } from "../../hooks/useSettings";
import { commands, RecordingRetentionPeriod } from "@/bindings";
import { notify } from "@/stores/noticeStore";

interface RecordingRetentionPeriodProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

// What happens to the recordings already saved when saving stops.
type ExistingRecordings = "keep" | "older" | "all";

const DEFAULT_OLDER_THAN_DAYS = 7;

export const RecordingRetentionPeriodSelector: React.FC<RecordingRetentionPeriodProps> =
  React.memo(({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();
    // How many recordings are saved now, while asking what to do with them.
    const [stopSavingCount, setStopSavingCount] = useState<number | null>(null);
    const [existing, setExisting] = useState<ExistingRecordings>("keep");
    const [days, setDays] = useState(String(DEFAULT_OLDER_THAN_DAYS));
    const [moving, setMoving] = useState(false);

    const selectedRetentionPeriod =
      getSetting("recording_retention_period") || "never";
    const historyLimit = getSetting("history_limit") || 5;

    const handleRetentionPeriodSelect = async (period: string) => {
      if (period === "dont_save" && selectedRetentionPeriod !== "dont_save") {
        await askStopSaving();
        return;
      }
      await updateSetting(
        "recording_retention_period",
        period as RecordingRetentionPeriod,
      );
    };

    const askStopSaving = async () => {
      try {
        const result = await commands.countRecordings();
        if (result.status !== "ok") throw new Error(result.error);
        if (result.data === 0) {
          await updateSetting("recording_retention_period", "dont_save");
          return;
        }
        setExisting("keep");
        setDays(String(DEFAULT_OLDER_THAN_DAYS));
        setStopSavingCount(result.data);
      } catch (error) {
        console.error("Failed to count recordings:", error);
        notify({
          title: t("settings.debug.recordingRetention.stopSaving.countFailed"),
        });
      }
    };

    const olderThanDays = Math.floor(Number(days));
    const daysValid = Number.isFinite(olderThanDays) && olderThanDays >= 1;

    const confirmStopSaving = async () => {
      if (existing !== "keep") {
        setMoving(true);
        try {
          const result = await commands.binRecordings(
            existing === "older" ? olderThanDays : null,
          );
          if (result.status !== "ok") throw new Error(result.error);
        } catch (error) {
          console.error("Failed to move recordings to the Bin:", error);
          notify({
            title: t("settings.debug.recordingRetention.stopSaving.moveFailed"),
          });
          return;
        } finally {
          setMoving(false);
        }
      }
      setStopSavingCount(null);
      await updateSetting("recording_retention_period", "dont_save");
    };

    const retentionOptions = [
      {
        value: "dont_save",
        label: t("settings.debug.recordingRetention.dontSave"),
      },
      { value: "never", label: t("settings.debug.recordingRetention.never") },
      {
        value: "preserve_limit",
        label: t("settings.debug.recordingRetention.preserveLimit", {
          count: Number(historyLimit),
        }),
      },
      { value: "days3", label: t("settings.debug.recordingRetention.days3") },
      { value: "weeks2", label: t("settings.debug.recordingRetention.weeks2") },
      {
        value: "months3",
        label: t("settings.debug.recordingRetention.months3"),
      },
    ];

    const choices: { value: ExistingRecordings; label: React.ReactNode }[] = [
      {
        value: "keep",
        label: t("settings.debug.recordingRetention.stopSaving.keep"),
      },
      {
        value: "older",
        label: (
          <Trans
            i18nKey="settings.debug.recordingRetention.stopSaving.older"
            components={{
              days: (
                <Input
                  type="number"
                  min={1}
                  step={1}
                  variant="compact"
                  className="mx-1 w-16"
                  aria-label={t(
                    "settings.debug.recordingRetention.stopSaving.days",
                  )}
                  value={days}
                  onChange={(event) => setDays(event.target.value)}
                  onFocus={() => setExisting("older")}
                  onClick={(event) => event.stopPropagation()}
                  onKeyDown={(event) => {
                    // Typing in the field mustn't pick the row.
                    if (event.key === " " || event.key === "Enter") {
                      event.stopPropagation();
                    }
                  }}
                />
              ),
            }}
          />
        ),
      },
      {
        value: "all",
        label: t("settings.debug.recordingRetention.stopSaving.all"),
      },
    ];

    const dialogOpen = stopSavingCount !== null;

    return (
      <SettingContainer
        title={t("settings.debug.recordingRetention.title")}
        description={t("settings.debug.recordingRetention.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      >
        <Dropdown
          options={retentionOptions}
          selectedValue={selectedRetentionPeriod}
          onSelect={handleRetentionPeriodSelect}
          placeholder={t("settings.debug.recordingRetention.placeholder")}
          disabled={isUpdating("recording_retention_period")}
        />
        <Dialog
          open={dialogOpen}
          title={t("settings.debug.recordingRetention.stopSaving.title")}
          description={t(
            "settings.debug.recordingRetention.stopSaving.description",
            { number: stopSavingCount ?? 0 },
          )}
          closeLabel={t("common.close")}
          dismissible={!moving}
          onOpenChange={(open) => {
            if (!open) setStopSavingCount(null);
          }}
          footer={
            <>
              <Button
                variant="secondary"
                disabled={moving}
                onClick={() => setStopSavingCount(null)}
              >
                {t("settings.debug.recordingRetention.stopSaving.cancel")}
              </Button>
              <Button
                variant="primary"
                disabled={moving || (existing === "older" && !daysValid)}
                onClick={() => void confirmStopSaving()}
              >
                {t("settings.debug.recordingRetention.stopSaving.confirm")}
              </Button>
            </>
          }
        >
          <div className="flex flex-col gap-3">
            <div
              role="radiogroup"
              aria-label={t(
                "settings.debug.recordingRetention.stopSaving.title",
              )}
              className="flex flex-col gap-1"
            >
              {choices.map((choice) => {
                const checked = existing === choice.value;
                return (
                  <div
                    key={choice.value}
                    role="radio"
                    aria-checked={checked}
                    tabIndex={checked ? 0 : -1}
                    onClick={() => setExisting(choice.value)}
                    onKeyDown={(event) => {
                      if (event.key === " " || event.key === "Enter") {
                        event.preventDefault();
                        setExisting(choice.value);
                      }
                    }}
                    className="flex min-h-9 cursor-pointer items-center gap-3 rounded-[4px] px-2 py-1.5 text-sm text-text hover:bg-wash focus:outline-none focus-visible:ring-2 focus-visible:ring-logo-primary"
                  >
                    <Radio checked={checked} />
                    <span className="flex flex-wrap items-center">
                      {choice.label}
                    </span>
                  </div>
                );
              })}
            </div>
            <p className="text-xs text-ink-3">
              {t("settings.debug.recordingRetention.stopSaving.note")}
            </p>
          </div>
        </Dialog>
      </SettingContainer>
    );
  });

RecordingRetentionPeriodSelector.displayName =
  "RecordingRetentionPeriodSelector";
