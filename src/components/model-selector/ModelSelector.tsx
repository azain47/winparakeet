import React, { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { commands } from "@/bindings";
import { getTranslatedModelName } from "../../lib/utils/modelTranslation";
import { useModelStore } from "../../stores/modelStore";
import { LANGUAGE_DETECTOR_ID } from "../../hooks/useLanguageDetector";
import {
  areModelsLocked,
  useModelsLocked,
} from "../../stores/transcriptionJobsStore";
import ModelStatusButton from "./ModelStatusButton";
import ModelDropdown from "./ModelDropdown";
import DownloadProgressDisplay from "./DownloadProgressDisplay";

import { ModelStateEvent } from "@/lib/types/events";

type ModelStatus =
  | "ready"
  | "loading"
  | "downloading"
  | "verifying"
  | "error"
  | "unloaded"
  | "none";

interface ModelSelectorProps {
  onError?: (error: string) => void;
}

const ModelSelector: React.FC<ModelSelectorProps> = ({ onError }) => {
  const { t } = useTranslation();
  const {
    models,
    currentModel,
    downloadProgress,
    verifyingModels,
    selectModel,
  } = useModelStore();
  const modelsLocked = useModelsLocked();

  const [modelStatus, setModelStatus] = useState<ModelStatus>("unloaded");
  const [modelError, setModelError] = useState<string | null>(null);
  const [showModelDropdown, setShowModelDropdown] = useState(false);
  // Track pending model switch for optimistic display
  const [pendingModelId, setPendingModelId] = useState<string | null>(null);

  const dropdownRef = useRef<HTMLDivElement>(null);

  const displayModelId = pendingModelId || currentModel;

  // Status polls and model events race; each bumps this, and a poll applies
  // only if nothing newer arrived while it awaited.
  const statusSeqRef = useRef(0);
  const currentModelRef = useRef(currentModel);
  currentModelRef.current = currentModel;

  const checkStatus = useCallback(async () => {
    const seq = ++statusSeqRef.current;
    const model = currentModelRef.current;
    if (!model) {
      setModelStatus("none");
      return;
    }
    try {
      const [statusResult, loading] = await Promise.all([
        commands.getTranscriptionModelStatus(),
        commands.isModelLoading(),
      ]);
      if (seq !== statusSeqRef.current) return;
      if (loading.status === "ok" && loading.data) {
        setModelStatus("loading");
      } else if (statusResult.status === "ok") {
        setModelStatus(statusResult.data === model ? "ready" : "unloaded");
      }
    } catch {
      if (seq !== statusSeqRef.current) return;
      setModelStatus("error");
      setModelError("Failed to check model status");
    }
  }, []);

  // Check model status when currentModel changes
  useEffect(() => {
    checkStatus();
  }, [currentModel, checkStatus]);

  useEffect(() => {
    let disposed = false;
    const timers = new Set<ReturnType<typeof setTimeout>>();

    // Listen for model loading lifecycle events
    const modelStateUnlisten = listen<ModelStateEvent>(
      "model-state-changed",
      (event) => {
        const { event_type, error } = event.payload;
        statusSeqRef.current++;
        switch (event_type) {
          case "loading_started":
            setModelStatus("loading");
            setModelError(null);
            break;
          case "loading_completed":
            setModelStatus("ready");
            setModelError(null);
            setPendingModelId(null);
            break;
          case "loading_failed":
            setModelStatus("error");
            setModelError(error || "Failed to load model");
            setPendingModelId(null);
            break;
          case "unloaded":
            // Not ordered against an in-flight load (the idle watcher may
            // unload the previous model), so ask the backend what is loaded.
            setModelError(null);
            checkStatus();
            break;
        }
      },
    );

    // Auto-select model when download completes
    const downloadCompleteUnlisten = listen<string>(
      "model-download-complete",
      (event) => {
        const modelId = event.payload;
        // The language detector downloads like a model but isn't one.
        if (modelId === LANGUAGE_DETECTOR_ID) return;
        const timer = setTimeout(async () => {
          timers.delete(timer);
          try {
            const isRecording = await commands.isRecording();
            // A file job holds the current model until it's done.
            if (disposed || isRecording || areModelsLocked()) return;
            setPendingModelId(modelId);
            setModelError(null);
            setShowModelDropdown(false);
            await selectModel(modelId);
            if (!disposed) setPendingModelId(null);
          } catch {
            // Ignore errors in auto-select
          }
        }, 500);
        timers.add(timer);
      },
    );

    // Click outside to close dropdown
    const handleClickOutside = (event: MouseEvent) => {
      if (
        dropdownRef.current &&
        !dropdownRef.current.contains(event.target as Node)
      ) {
        setShowModelDropdown(false);
      }
    };

    document.addEventListener("mousedown", handleClickOutside);

    return () => {
      disposed = true;
      timers.forEach(clearTimeout);
      document.removeEventListener("mousedown", handleClickOutside);
      modelStateUnlisten.then((fn) => fn());
      downloadCompleteUnlisten.then((fn) => fn());
    };
  }, [selectModel, checkStatus]);

  const handleModelSelect = async (modelId: string) => {
    setPendingModelId(modelId);
    setModelError(null);
    setShowModelDropdown(false);
    const success = await selectModel(modelId);
    // On success the store's currentModel is the pick; nothing stays pending
    // even when no load events follow (e.g. it was already loaded).
    setPendingModelId(null);
    if (success) {
      checkStatus();
    } else {
      setModelStatus("error");
      setModelError("Failed to switch model");
      onError?.("Failed to switch model");
    }
  };

  const getModelDisplayText = (): string => {
    const verifyingKeys = Object.keys(verifyingModels);
    if (verifyingKeys.length > 0) {
      if (verifyingKeys.length === 1) {
        const modelId = verifyingKeys[0];
        const model = models.find((m) => m.id === modelId);
        const modelName = model
          ? getTranslatedModelName(model, t)
          : t("modelSelector.verifyingGeneric").replace("...", "");
        return t("modelSelector.verifying", { modelName });
      } else {
        return t("modelSelector.verifyingGeneric");
      }
    }

    const progressValues = Object.values(downloadProgress);
    if (progressValues.length > 0) {
      if (progressValues.length === 1) {
        const progress = progressValues[0];
        const percentage = Math.max(
          0,
          Math.min(100, Math.round(progress.percentage)),
        );
        return t("modelSelector.downloading", { percentage });
      } else {
        return t("modelSelector.downloadingMultiple", {
          count: progressValues.length,
        });
      }
    }

    const currentModelInfo = models.find((m) => m.id === displayModelId);

    switch (modelStatus) {
      case "ready":
        return currentModelInfo
          ? getTranslatedModelName(currentModelInfo, t)
          : t("modelSelector.modelReady");
      case "loading":
        return currentModelInfo
          ? t("modelSelector.loading", {
              modelName: getTranslatedModelName(currentModelInfo, t),
            })
          : t("modelSelector.loadingGeneric");
      case "error":
        return modelError || t("modelSelector.modelError");
      case "unloaded":
        return currentModelInfo
          ? getTranslatedModelName(currentModelInfo, t)
          : t("modelSelector.modelUnloaded");
      case "none":
        return t("modelSelector.noModelDownloadRequired");
      default:
        return currentModelInfo
          ? getTranslatedModelName(currentModelInfo, t)
          : t("modelSelector.modelUnloaded");
    }
  };

  // Derive display status from model status + store state
  const getDisplayStatus = (): ModelStatus => {
    if (Object.keys(verifyingModels).length > 0) return "verifying";
    if (Object.keys(downloadProgress).length > 0) return "downloading";
    return modelStatus;
  };

  return (
    <>
      {/* Model Status and Switcher */}
      <div className="relative" ref={dropdownRef}>
        <ModelStatusButton
          status={getDisplayStatus()}
          displayText={getModelDisplayText()}
          isDropdownOpen={showModelDropdown}
          busy={modelsLocked}
          onClick={() => setShowModelDropdown(!showModelDropdown)}
        />

        {/* Model Dropdown */}
        {showModelDropdown && (
          <ModelDropdown
            models={models}
            currentModelId={displayModelId}
            locked={modelsLocked}
            onModelSelect={handleModelSelect}
          />
        )}
      </div>

      {/* Download Progress Bar for Models */}
      <DownloadProgressDisplay downloadProgress={downloadProgress} />
    </>
  );
};

export default ModelSelector;
