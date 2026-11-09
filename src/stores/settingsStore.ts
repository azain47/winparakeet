import { create } from "zustand";
import { subscribeWithSelector } from "zustand/middleware";
import { listen } from "@tauri-apps/api/event";
import type {
  AppSettings as Settings,
  AudioDevice,
  PostProcessApiFormat,
  ChineseScript,
  TranscribeAcceleratorSetting,
  ShortcutActivation,
  VadBackend,
  LanguageDetectionMode,
  MeetingSources,
  MeetingAudioRetention,
} from "@/bindings";
import { commands } from "@/bindings";
import { notify } from "@/stores/noticeStore";

interface SettingsStore {
  settings: Settings | null;
  defaultSettings: Settings | null;
  isLoading: boolean;
  // In-flight writes per key; a key is updating while its count is above 0.
  isUpdating: Record<string, number>;
  audioDevices: AudioDevice[];
  outputDevices: AudioDevice[];
  customSounds: { start: boolean; stop: boolean };
  postProcessModelOptions: Record<string, string[]>;
  // null until loadUpdateChecksLocked() resolves
  updateChecksLocked: boolean | null;

  // Actions
  initialize: () => Promise<void>;
  loadDefaultSettings: () => Promise<void>;
  loadUpdateChecksLocked: () => Promise<void>;
  /** Resolves to whether the backend accepted the value. */
  updateSetting: <K extends keyof Settings>(
    key: K,
    value: Settings[K],
  ) => Promise<boolean>;
  resetSetting: (key: keyof Settings) => Promise<boolean>;
  refreshSettings: () => Promise<void>;
  refreshAudioDevices: () => Promise<void>;
  refreshOutputDevices: () => Promise<void>;
  updateBinding: (id: string, binding: string) => Promise<void>;
  resetBinding: (id: string) => Promise<void>;
  getSetting: <K extends keyof Settings>(key: K) => Settings[K] | undefined;
  isUpdatingKey: (key: string) => boolean;
  playTestSound: (soundType: "start" | "stop") => Promise<void>;
  checkCustomSounds: () => Promise<void>;
  setPostProcessProvider: (providerId: string) => Promise<void>;
  updatePostProcessSetting: (
    settingType: "base_url" | "api_key" | "model",
    providerId: string,
    value: string,
  ) => Promise<void>;
  updatePostProcessBaseUrl: (
    providerId: string,
    baseUrl: string,
  ) => Promise<void>;
  updatePostProcessApiFormat: (
    providerId: string,
    apiFormat: PostProcessApiFormat,
  ) => Promise<void>;
  updatePostProcessApiKey: (
    providerId: string,
    apiKey: string,
  ) => Promise<void>;
  updatePostProcessModel: (providerId: string, model: string) => Promise<void>;
  fetchPostProcessModels: (providerId: string) => Promise<string[]>;
  setPostProcessModelOptions: (providerId: string, models: string[]) => void;

  // Internal state setters
  setSettings: (settings: Settings | null) => void;
  setDefaultSettings: (defaultSettings: Settings | null) => void;
  setLoading: (loading: boolean) => void;
  /** Marks one write to `key` as started (true) or ended (false). */
  setUpdating: (key: string, updating: boolean) => void;
  setAudioDevices: (devices: AudioDevice[]) => void;
  setOutputDevices: (devices: AudioDevice[]) => void;
  setCustomSounds: (sounds: { start: boolean; stop: boolean }) => void;
}

// Note: Default settings are now fetched from Rust via commands.getDefaultSettings()
// This ensures platform-specific defaults (like overlay_position, shortcuts, paste_method) work correctly

const DEFAULT_AUDIO_DEVICE: AudioDevice = {
  index: "default",
  name: "Default",
  is_default: true,
};

const isErrorResult = (
  result: unknown,
): result is { status: "error"; error: string } =>
  typeof result === "object" &&
  result !== null &&
  (result as { status?: unknown }).status === "error";

const settingUpdaters: {
  [K in keyof Settings]?: (value: Settings[K]) => Promise<unknown>;
} = {
  always_on_microphone: (value) =>
    commands.updateMicrophoneMode(value as boolean),
  audio_feedback: (value) =>
    commands.changeAudioFeedbackSetting(value as boolean),
  audio_feedback_volume: (value) =>
    commands.changeAudioFeedbackVolumeSetting(value as number),
  sound_theme: (value) => commands.changeSoundThemeSetting(value as string),
  start_hidden: (value) => commands.changeStartHiddenSetting(value as boolean),
  autostart_enabled: (value) =>
    commands.changeAutostartSetting(value as boolean),
  spoken_languages: (value) =>
    commands.changeSpokenLanguagesSetting(value as string[]),
  update_checks_enabled: (value) =>
    commands.changeUpdateChecksSetting(value as boolean),
  show_whats_new_on_update: (value) =>
    commands.changeShowWhatsNewOnUpdateSetting(value as boolean),
  whats_new_last_seen_version: (value) =>
    commands.changeWhatsNewLastSeenVersionSetting(value as string),
  shortcut_activation: (value) =>
    commands.changeShortcutActivationSetting(value as ShortcutActivation),
  hold_threshold_ms: (value) =>
    commands.changeHoldThresholdMsSetting(value as number),
  selected_microphone: (value) =>
    commands.setSelectedMicrophone(
      (value as string) === "Default" || value === null
        ? "default"
        : (value as string),
    ),
  selected_channel: async (value) => {
    const result = await commands.setSelectedChannel(
      (value as number | null | undefined) ?? null,
    );
    if (result.status === "error") {
      throw new Error(result.error);
    }
  },
  clamshell_microphone: (value) =>
    commands.setClamshellMicrophone(
      (value as string) === "Default" ? "default" : (value as string),
    ),
  selected_output_device: (value) =>
    commands.setSelectedOutputDevice(
      (value as string) === "Default" || value === null
        ? "default"
        : (value as string),
    ),
  recording_retention_period: (value) =>
    commands.updateRecordingRetentionPeriod(value as string),
  meeting_sources: (value) =>
    commands.changeMeetingSourcesSetting(value as MeetingSources),
  meeting_model: (value) =>
    commands.changeMeetingModelSetting((value as string | null) ?? null),
  meeting_speaker_labels: (value) =>
    commands.changeMeetingSpeakerLabelsSetting(value as boolean),
  meeting_audio_retention: (value) =>
    commands.changeMeetingAudioRetentionSetting(value as MeetingAudioRetention),
  translate_to_english: (value) =>
    commands.changeTranslateToEnglishSetting(value as boolean),
  selected_language: (value) =>
    commands.changeSelectedLanguageSetting(value as string),
  overlay_position: (value) =>
    commands.changeOverlayPositionSetting(value as string),
  debug_mode: (value) => commands.changeDebugModeSetting(value as boolean),
  custom_words: (value) => commands.updateCustomWords(value as string[]),
  word_correction_threshold: (value) =>
    commands.changeWordCorrectionThresholdSetting(value as number),
  paste_delay_ms: (value) =>
    commands.changePasteDelayMsSetting(value as number),
  paste_delay_after_ms: (value) =>
    commands.changePasteDelayAfterMsSetting(value as number),
  reliable_paste: (value) =>
    commands.changeReliablePasteSetting(value as boolean),
  paste_method: (value) => commands.changePasteMethodSetting(value as string),
  typing_tool: (value) => commands.changeTypingToolSetting(value as string),
  external_script_path: (value) =>
    commands.changeExternalScriptPathSetting(value as string | null),
  clipboard_handling: (value) =>
    commands.changeClipboardHandlingSetting(value as string),
  auto_submit: (value) => commands.changeAutoSubmitSetting(value as boolean),
  auto_submit_key: (value) =>
    commands.changeAutoSubmitKeySetting(value as string),
  history_limit: (value) => commands.updateHistoryLimit(value as number),
  post_process_enabled: (value) =>
    commands.changePostProcessEnabledSetting(value as boolean),
  post_process_selected_prompt_id: (value) =>
    commands.setPostProcessSelectedPrompt(value as string),
  mute_while_recording: (value) =>
    commands.changeMuteWhileRecordingSetting(value as boolean),
  append_trailing_space: (value) =>
    commands.changeAppendTrailingSpaceSetting(value as boolean),
  log_level: (value) => commands.setLogLevel(value as any),
  app_language: (value) => commands.changeAppLanguageSetting(value as string),
  theme: (value) => commands.changeThemeSetting(value as string),
  experimental_enabled: (value) =>
    commands.changeExperimentalEnabledSetting(value as boolean),
  lazy_stream_close: (value) =>
    commands.changeLazyStreamCloseSetting(value as boolean),
  airpods_mode: async (value) => {
    const result = await commands.changeAirpodsModeSetting(value as boolean);
    if (result.status === "error") {
      // Rejected changes (e.g. mid-recording) roll the toggle back via the
      // throw below; the notice tells the user why.
      notify({ title: result.error });
      throw new Error(result.error);
    }
  },
  overlay_style: (value) => commands.changeOverlayStyleSetting(value as string),
  vad_enabled: (value) => commands.changeVadEnabledSetting(value as boolean),
  long_form_chunking: (value) =>
    commands.changeLongFormChunkingSetting(value as boolean),
  vad_backend: async (value) => {
    const result = await commands.changeVadBackendSetting(value as VadBackend);
    if (result.status === "error") {
      // Rejected switches (e.g. mid-recording) roll the dropdown back via the
      // throw below; the notice tells the user why.
      notify({ title: result.error });
      throw new Error(result.error);
    }
  },
  filler_word_removal_enabled: (value) =>
    commands.changeFillerWordRemovalEnabledSetting(value as boolean),
  chinese_script: (value) =>
    commands.changeChineseScriptSetting(value as ChineseScript),
  show_tray_icon: (value) =>
    commands.changeShowTrayIconSetting(value as boolean),
  transcribe_accelerator: (value) =>
    commands.changeTranscribeAcceleratorSetting(
      value as TranscribeAcceleratorSetting,
    ),
  transcribe_gpu_device: (value) =>
    commands.changeTranscribeGpuDevice(value as string | null),
  extra_recording_buffer_ms: (value) =>
    commands.changeExtraRecordingBufferSetting(value as number),
  language_detection: (value) =>
    commands.changeLanguageDetectionSetting(value as LanguageDetectionMode),
  language_detection_window_secs: (value) =>
    commands.changeLanguageDetectionWindowSetting(value as number),
};

export const useSettingsStore = create<SettingsStore>()(
  subscribeWithSelector((set, get) => ({
    settings: null,
    defaultSettings: null,
    isLoading: true,
    isUpdating: {},
    audioDevices: [],
    outputDevices: [],
    customSounds: { start: false, stop: false },
    postProcessModelOptions: {},
    updateChecksLocked: null,

    // Internal setters
    setSettings: (settings) => set({ settings }),
    setDefaultSettings: (defaultSettings) => set({ defaultSettings }),
    setLoading: (isLoading) => set({ isLoading }),
    setUpdating: (key, updating) =>
      set((state) => {
        const count = Math.max(
          0,
          (state.isUpdating[key] ?? 0) + (updating ? 1 : -1),
        );
        const isUpdating = { ...state.isUpdating, [key]: count };
        if (!count) delete isUpdating[key];
        return { isUpdating };
      }),
    setAudioDevices: (audioDevices) => set({ audioDevices }),
    setOutputDevices: (outputDevices) => set({ outputDevices }),
    setCustomSounds: (customSounds) => set({ customSounds }),

    // Getters
    getSetting: (key) => get().settings?.[key],
    isUpdatingKey: (key) => (get().isUpdating[key] ?? 0) > 0,

    // Load settings from store
    refreshSettings: async () => {
      try {
        const result = await commands.getAppSettings();
        if (result.status === "ok") {
          const settings = result.data;
          const normalizedSettings: Settings = {
            ...settings,
            always_on_microphone: settings.always_on_microphone ?? false,
            selected_microphone: settings.selected_microphone ?? "Default",
            clamshell_microphone: settings.clamshell_microphone ?? "Default",
            selected_output_device:
              settings.selected_output_device ?? "Default",
          };
          set({ settings: normalizedSettings, isLoading: false });
        } else {
          console.error("Failed to load settings:", result.error);
          set({ isLoading: false });
        }
      } catch (error) {
        console.error("Failed to load settings:", error);
        set({ isLoading: false });
      }
    },

    // Load audio devices
    refreshAudioDevices: async () => {
      try {
        const result = await commands.getAvailableMicrophones();
        if (result.status === "ok") {
          const devicesWithDefault = [
            DEFAULT_AUDIO_DEVICE,
            ...result.data.filter(
              (d) => d.name !== "Default" && d.name !== "default",
            ),
          ];
          set({ audioDevices: devicesWithDefault });
        } else {
          set({ audioDevices: [DEFAULT_AUDIO_DEVICE] });
        }
      } catch (error) {
        console.error("Failed to load audio devices:", error);
        set({ audioDevices: [DEFAULT_AUDIO_DEVICE] });
      }
    },

    // Load output devices
    refreshOutputDevices: async () => {
      try {
        const result = await commands.getAvailableOutputDevices();
        if (result.status === "ok") {
          const devicesWithDefault = [
            DEFAULT_AUDIO_DEVICE,
            ...result.data.filter(
              (d) => d.name !== "Default" && d.name !== "default",
            ),
          ];
          set({ outputDevices: devicesWithDefault });
        } else {
          set({ outputDevices: [DEFAULT_AUDIO_DEVICE] });
        }
      } catch (error) {
        console.error("Failed to load output devices:", error);
        set({ outputDevices: [DEFAULT_AUDIO_DEVICE] });
      }
    },

    // Play a test sound
    playTestSound: async (soundType: "start" | "stop") => {
      try {
        await commands.playTestSound(soundType);
      } catch (error) {
        console.error(`Failed to play test sound (${soundType}):`, error);
      }
    },

    checkCustomSounds: async () => {
      try {
        const sounds = await commands.checkCustomSounds();
        get().setCustomSounds(sounds);
      } catch (error) {
        console.error("Failed to check custom sounds:", error);
      }
    },

    // Update a specific setting
    updateSetting: async <K extends keyof Settings>(
      key: K,
      value: Settings[K],
    ) => {
      const { setUpdating } = get();
      const updateKey = String(key);
      const originalValue = get().settings?.[key];

      setUpdating(updateKey, true);
      set((state) => ({
        settings: state.settings ? { ...state.settings, [key]: value } : null,
      }));

      try {
        const updater = settingUpdaters[key];
        if (updater) {
          const result = await updater(value);
          // Most commands report failure as a Result rather than a throw.
          if (isErrorResult(result)) throw new Error(result.error);
        } else if (key !== "bindings" && key !== "selected_model") {
          console.warn(`No handler for setting: ${String(key)}`);
        }
        return true;
      } catch (error) {
        console.error(`Failed to update setting ${String(key)}:`, error);
        // Some commands save the value and fail afterwards (a cleanup step),
        // so the backend's stored value, not the one from before this call,
        // is what to restore. Fall back to the old value if it can't be read.
        let restored = originalValue;
        try {
          const stored = await commands.getAppSettings();
          if (stored.status === "ok") restored = stored.data[key] ?? restored;
        } catch {
          // Keep the value from before this call.
        }
        // Roll back only this key, and only while it still holds this call's
        // value: other keys and later writes to this key stay untouched.
        set((state) =>
          state.settings && state.settings[key] === value
            ? { settings: { ...state.settings, [key]: restored } }
            : {},
        );
        return false;
      } finally {
        setUpdating(updateKey, false);
      }
    },

    // Reset a setting to its default value
    resetSetting: async (key) => {
      const { defaultSettings } = get();
      if (defaultSettings) {
        const defaultValue = defaultSettings[key];
        if (defaultValue !== undefined) {
          return get().updateSetting(key, defaultValue as any);
        }
      }
      return false;
    },

    // Update a specific binding
    updateBinding: async (id, binding) => {
      const { setUpdating } = get();
      const updateKey = `binding_${id}`;
      const original = get().settings?.bindings?.[id];

      // Writes this binding's current_binding, if the binding exists.
      const setCurrent = (current: string) =>
        set((state) => {
          const existing = state.settings?.bindings?.[id];
          if (!state.settings || !existing) return {};
          return {
            settings: {
              ...state.settings,
              bindings: {
                ...state.settings.bindings,
                [id]: { ...existing, current_binding: current },
              },
            },
          };
        });

      setUpdating(updateKey, true);

      try {
        // Optimistic update
        setCurrent(binding);

        const result = await commands.changeBinding(id, binding);

        // Check if the command executed successfully
        if (result.status === "error") {
          throw new Error(result.error);
        }

        // Check if the binding change was successful
        if (!result.data.success) {
          throw new Error(result.data.error || "Failed to update binding");
        }
      } catch (error) {
        console.error(`Failed to update binding ${id}:`, error);

        // Roll back (an empty original included) unless a later write has
        // already replaced this call's value.
        if (
          original &&
          get().settings?.bindings?.[id]?.current_binding === binding
        ) {
          setCurrent(original.current_binding);
        }

        // Re-throw to let the caller know it failed
        throw error;
      } finally {
        setUpdating(updateKey, false);
      }
    },

    // Reset a specific binding
    resetBinding: async (id) => {
      const { setUpdating, refreshSettings } = get();
      const updateKey = `binding_${id}`;

      setUpdating(updateKey, true);

      try {
        await commands.resetBinding(id);
        await refreshSettings();
      } catch (error) {
        console.error(`Failed to reset binding ${id}:`, error);
      } finally {
        setUpdating(updateKey, false);
      }
    },

    setPostProcessProvider: async (providerId) => {
      const {
        settings,
        setUpdating,
        refreshSettings,
        setPostProcessModelOptions,
      } = get();
      const updateKey = "post_process_provider_id";
      const previousId = settings?.post_process_provider_id ?? null;

      setUpdating(updateKey, true);

      if (settings) {
        set((state) => ({
          settings: state.settings
            ? { ...state.settings, post_process_provider_id: providerId }
            : null,
        }));
      }

      // Clear cached model options for the new provider so the dropdown
      // doesn't show stale models from a previous fetch or base_url.
      setPostProcessModelOptions(providerId, []);

      try {
        await commands.setPostProcessProvider(providerId);
        await refreshSettings();
      } catch (error) {
        console.error("Failed to set post-process provider:", error);
        if (previousId !== null) {
          set((state) => ({
            settings: state.settings
              ? { ...state.settings, post_process_provider_id: previousId }
              : null,
          }));
        }
      } finally {
        setUpdating(updateKey, false);
      }
    },

    // Generic updater for post-processing provider settings
    updatePostProcessSetting: async (
      settingType: "base_url" | "api_key" | "model",
      providerId: string,
      value: string,
    ) => {
      const { setUpdating, refreshSettings } = get();
      const updateKey = `post_process_${settingType}:${providerId}`;

      setUpdating(updateKey, true);

      try {
        if (settingType === "base_url") {
          await commands.changePostProcessBaseUrlSetting(providerId, value);
        } else if (settingType === "api_key") {
          await commands.changePostProcessApiKeySetting(providerId, value);
        } else if (settingType === "model") {
          await commands.changePostProcessModelSetting(providerId, value);
        }
        await refreshSettings();
      } catch (error) {
        console.error(
          `Failed to update post-process ${settingType.replace("_", " ")}:`,
          error,
        );
      } finally {
        setUpdating(updateKey, false);
      }
    },

    updatePostProcessBaseUrl: async (providerId, baseUrl) => {
      const { setUpdating, refreshSettings } = get();
      const updateKey = `post_process_base_url:${providerId}`;

      setUpdating(updateKey, true);

      try {
        // Persist the new base URL first.
        const urlResult = await commands.changePostProcessBaseUrlSetting(
          providerId,
          baseUrl,
        );
        if (urlResult.status === "error") {
          console.error("Failed to persist base URL:", urlResult.error);
          return;
        }

        // Reset the stored model since the previous value is almost certainly
        // invalid for the new endpoint (e.g. switching Custom from Groq to
        // Cerebras). Only proceed if the reset succeeds.
        const modelResult = await commands.changePostProcessModelSetting(
          providerId,
          "",
        );
        if (modelResult.status === "error") {
          console.error("Failed to reset model setting:", modelResult.error);
          return;
        }

        // Clear cached model options only after both backend writes succeed.
        set((state) => ({
          postProcessModelOptions: {
            ...state.postProcessModelOptions,
            [providerId]: [],
          },
        }));

        // Single refresh after both backend writes.
        await refreshSettings();
      } catch (error) {
        console.error("Failed to update post-process base URL:", error);
      } finally {
        setUpdating(updateKey, false);
      }
    },

    updatePostProcessApiFormat: async (providerId, apiFormat) => {
      const { setUpdating, refreshSettings } = get();
      const updateKey = `post_process_api_format:${providerId}`;

      // The format decides the auth headers, so models listed under the old
      // format may not reflect this one - user should click refresh after
      set((state) => ({
        postProcessModelOptions: {
          ...state.postProcessModelOptions,
          [providerId]: [],
        },
      }));
      setUpdating(updateKey, true);

      try {
        const result = await commands.changePostProcessApiFormatSetting(
          providerId,
          apiFormat,
        );
        if (result.status === "error") {
          console.error("Failed to update API format:", result.error);
          return;
        }
        await refreshSettings();
      } catch (error) {
        console.error("Failed to update post-process API format:", error);
      } finally {
        setUpdating(updateKey, false);
      }
    },

    updatePostProcessApiKey: async (providerId, apiKey) => {
      // Clear cached models when API key changes - user should click refresh after
      set((state) => ({
        postProcessModelOptions: {
          ...state.postProcessModelOptions,
          [providerId]: [],
        },
      }));
      return get().updatePostProcessSetting("api_key", providerId, apiKey);
    },

    updatePostProcessModel: async (providerId, model) => {
      return get().updatePostProcessSetting("model", providerId, model);
    },

    fetchPostProcessModels: async (providerId) => {
      const updateKey = `post_process_models_fetch:${providerId}`;
      const { setUpdating, setPostProcessModelOptions } = get();

      setUpdating(updateKey, true);

      try {
        // Call Tauri backend command instead of fetch
        const result = await commands.fetchPostProcessModels(providerId);
        if (result.status === "ok") {
          setPostProcessModelOptions(providerId, result.data);
          return result.data;
        } else {
          console.error("Failed to fetch models:", result.error);
          return [];
        }
      } catch (error) {
        console.error("Failed to fetch models:", error);
        // Don't cache empty array on error - let user retry
        return [];
      } finally {
        setUpdating(updateKey, false);
      }
    },

    setPostProcessModelOptions: (providerId, models) =>
      set((state) => ({
        postProcessModelOptions: {
          ...state.postProcessModelOptions,
          [providerId]: models,
        },
      })),

    // Load default settings from Rust
    loadDefaultSettings: async () => {
      try {
        const result = await commands.getDefaultSettings();
        if (result.status === "ok") {
          set({ defaultSettings: result.data });
        } else {
          console.error("Failed to load default settings:", result.error);
        }
      } catch (error) {
        console.error("Failed to load default settings:", error);
      }
    },

    // Check whether update checks are locked by system configuration
    // (e.g. HANDY_DISABLE_UPDATER, set by the Nix package)
    loadUpdateChecksLocked: async () => {
      try {
        const locked = await commands.isUpdateChecksLocked();
        set({ updateChecksLocked: locked });
      } catch (error) {
        console.error("Failed to check update checks lock state:", error);
        // Fail open: an unknown lock state means "not locked", otherwise the
        // update checker waits for it forever and checks never start.
        set({ updateChecksLocked: false });
      }
    },

    // Initialize everything
    initialize: async () => {
      const {
        refreshSettings,
        checkCustomSounds,
        loadDefaultSettings,
        loadUpdateChecksLocked,
      } = get();

      // Note: Audio devices are NOT refreshed here. The frontend (App.tsx)
      // is responsible for calling refreshAudioDevices/refreshOutputDevices
      // after onboarding completes. This avoids triggering permission dialogs
      // on macOS before the user is ready.
      await Promise.all([
        loadDefaultSettings(),
        refreshSettings(),
        checkCustomSounds(),
        loadUpdateChecksLocked(),
      ]);

      // Re-fetch settings when the backend changes them (e.g. language
      // reset during model switch). The backend is the source of truth.
      listen("model-state-changed", () => {
        get().refreshSettings();
      });
      listen<{ setting?: string }>("settings-changed", (event) => {
        get().refreshSettings();
        if (event.payload.setting === "selected_microphone") {
          get().refreshAudioDevices();
        }
      });
    },
  })),
);
