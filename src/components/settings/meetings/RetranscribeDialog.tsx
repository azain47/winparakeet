import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { commands } from "@/bindings";
import { useSettings } from "@/hooks/useSettings";
import { useModelStore } from "@/stores/modelStore";
import { notify } from "@/stores/noticeStore";
import { Button } from "../../ui/Button";
import { Dialog } from "../../ui/Dialog";
import { Radio } from "../../ui/Radio";
import { useModelOptions } from "./useModelOptions";

interface RetranscribeDialogProps {
  meetingId: number;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onOpenModels: () => void;
}

/** Picks the model to transcribe a meeting again with. */
export const RetranscribeDialog: React.FC<RetranscribeDialogProps> = ({
  meetingId,
  open,
  onOpenChange,
  onOpenModels,
}) => {
  const { t } = useTranslation();
  const { getSetting } = useSettings();
  const meetingModel = getSetting("meeting_model") ?? null;
  const dictationModel = useModelStore((state) => state.currentModel);
  // A one-off choice, so the models are listed once each, by name; it
  // starts on the model meetings are set to use.
  const options = useModelOptions(meetingModel, false);
  const initial = meetingModel ?? dictationModel ?? "";
  const [choice, setChoice] = useState(initial);
  const [busy, setBusy] = useState(false);

  // Each time it opens it starts from the setting.
  useEffect(() => {
    if (open) setChoice(initial);
  }, [open, initial]);

  const transcribe = async () => {
    // `null` (nothing to choose from) leaves it to the meeting setting.
    const modelId = choice || null;
    setBusy(true);
    try {
      const result = await commands.transcribeMeeting(meetingId, modelId);
      if (result.status !== "ok") throw new Error(result.error);
      onOpenChange(false);
    } catch (error) {
      console.error("Failed to queue the meeting's transcription:", error);
      notify({
        title: t("meetings.detail.transcribeFailed"),
        detail: error instanceof Error ? error.message : String(error),
      });
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      open={open}
      title={t("meetings.retranscribe.title")}
      closeLabel={t("common.close")}
      dismissible={!busy}
      onOpenChange={onOpenChange}
      footer={
        <>
          <Button
            variant="secondary"
            disabled={busy}
            onClick={() => onOpenChange(false)}
          >
            {t("meetings.cancel")}
          </Button>
          <Button
            variant="primary"
            disabled={busy}
            onClick={() => void transcribe()}
          >
            {t("meetings.retranscribe.confirm")}
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-3">
        <div
          role="radiogroup"
          aria-label={t("meetings.retranscribe.model")}
          className="flex flex-col gap-1"
        >
          {options.map((option) => {
            const checked = choice === option.value;
            return (
              <div
                key={option.value}
                role="radio"
                aria-checked={checked}
                tabIndex={checked ? 0 : -1}
                onClick={() => setChoice(option.value)}
                onKeyDown={(event) => {
                  if (event.key === " " || event.key === "Enter") {
                    event.preventDefault();
                    setChoice(option.value);
                  }
                }}
                className="flex min-h-9 cursor-pointer items-center gap-3 rounded-[4px] px-2 py-1.5 text-sm text-text hover:bg-wash focus:outline-none focus-visible:ring-2 focus-visible:ring-logo-primary"
              >
                <Radio checked={checked} />
                <span>{option.label}</span>
              </div>
            );
          })}
        </div>
        <p className="text-xs text-ink-2">{t("meetings.retranscribe.hint")}</p>
        <p className="text-xs text-ink-2">
          {t("meetings.retranscribe.moreModels")}{" "}
          <button
            type="button"
            onClick={() => {
              onOpenChange(false);
              onOpenModels();
            }}
            className="cursor-pointer font-medium text-logo-primary hover:underline focus:outline-none focus-visible:ring-1 focus-visible:ring-logo-primary"
          >
            {t("meetings.retranscribe.openModels")}
          </button>
        </p>
      </div>
    </Dialog>
  );
};
