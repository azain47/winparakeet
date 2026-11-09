import { useTranslation } from "react-i18next";
import { useModelStore } from "@/stores/modelStore";
import { getTranslatedModelName } from "@/lib/utils/modelTranslation";
import type { DropdownOption } from "../../ui/Dropdown";

/** The "Same as dictation" choice's value; it stands for `null`. */
export const SAME_AS_DICTATION = "";

/**
 * Models a meeting can be transcribed with: "Same as dictation", then every
 * downloaded model. `keep` stays listed even when it isn't downloaded, so a
 * chosen model that was deleted still shows by name.
 */
export const useModelOptions = (keep?: string | null): DropdownOption[] => {
  const { t } = useTranslation();
  const models = useModelStore((state) => state.models);
  const currentModel = useModelStore((state) => state.currentModel);
  const current = models.find((m) => m.id === currentModel);

  return [
    {
      value: SAME_AS_DICTATION,
      label: current
        ? t("meetings.model.sameAsDictationNamed", {
            model: getTranslatedModelName(current, t),
          })
        : t("meetings.model.sameAsDictation"),
    },
    ...models
      .filter((m) => m.is_downloaded || m.id === keep)
      .map((m) => ({ value: m.id, label: getTranslatedModelName(m, t) })),
  ];
};
