import React from "react";
import { useTranslation } from "react-i18next";
import {
  Cog,
  FlaskConical,
  History,
  Info,
  Mic,
  Sparkles,
  Cpu,
  Trash2,
  Users,
} from "lucide-react";
import AppMark from "./icons/AppMark";
import { PRODUCT_NAME } from "../lib/brand";
import { useSettings } from "../hooks/useSettings";
import { useMeetingRecordingActive } from "../stores/meetingsStore";
import {
  GeneralSettings,
  AdvancedSettings,
  HistorySettings,
  BinSettings,
  AboutSettings,
  PostProcessingSettings,
  ModelsSettings,
} from "./settings";

export type SidebarSection = keyof typeof SECTIONS_CONFIG;

interface IconProps {
  width?: number | string;
  height?: number | string;
  size?: number | string;
  className?: string;
  [key: string]: any;
}

interface SectionConfig {
  labelKey: string;
  icon: React.ComponentType<IconProps>;
  /** `null` when App renders the section itself (Meetings and Debug, with their handlers). */
  component: React.ComponentType | null;
  enabled: (settings: any) => boolean;
}

export const SECTIONS_CONFIG = {
  general: {
    labelKey: "sidebar.general",
    icon: Mic,
    component: GeneralSettings,
    enabled: () => true,
  },
  history: {
    labelKey: "sidebar.history",
    icon: History,
    component: HistorySettings,
    enabled: () => true,
  },
  meetings: {
    labelKey: "sidebar.meetings",
    icon: Users,
    component: null,
    enabled: () => true,
  },
  bin: {
    labelKey: "sidebar.bin",
    icon: Trash2,
    component: BinSettings,
    enabled: () => true,
  },
  models: {
    labelKey: "sidebar.models",
    icon: Cpu,
    component: ModelsSettings,
    enabled: () => true,
  },
  advanced: {
    labelKey: "sidebar.advanced",
    icon: Cog,
    component: AdvancedSettings,
    enabled: () => true,
  },
  postprocessing: {
    labelKey: "sidebar.postProcessing",
    icon: Sparkles,
    component: PostProcessingSettings,
    enabled: (settings) => settings?.post_process_enabled ?? false,
  },
  debug: {
    labelKey: "sidebar.debug",
    icon: FlaskConical,
    component: null,
    enabled: (settings) => settings?.debug_mode ?? false,
  },
  about: {
    labelKey: "sidebar.about",
    icon: Info,
    component: AboutSettings,
    enabled: () => true,
  },
} as const satisfies Record<string, SectionConfig>;

interface SidebarProps {
  activeSection: SidebarSection;
  onSectionChange: (section: SidebarSection) => void;
}

export const Sidebar: React.FC<SidebarProps> = ({
  activeSection,
  onSectionChange,
}) => {
  const { t } = useTranslation();
  const { settings } = useSettings();
  const meetingRecording = useMeetingRecordingActive();

  const availableSections = Object.entries(SECTIONS_CONFIG)
    .filter(([_, config]) => config.enabled(settings))
    .map(([id, config]) => ({ id: id as SidebarSection, ...config }));

  return (
    <nav className="flex h-full w-52 shrink-0 flex-col px-2 pb-2">
      <div className="flex h-14 items-center gap-2.5 px-3">
        <AppMark className="size-[22px] shrink-0" />
        <span className="truncate font-display text-[15px] font-semibold tracking-[-0.01em]">
          {PRODUCT_NAME}
        </span>
      </div>
      <div className="flex flex-col gap-0.5">
        {availableSections.map((section) => {
          const Icon = section.icon;
          const isActive = activeSection === section.id;

          return (
            <button
              key={section.id}
              type="button"
              aria-current={isActive ? "page" : undefined}
              title={t(section.labelKey)}
              className={`relative flex h-9 w-full cursor-pointer items-center gap-3 rounded-[4px] px-3 text-start text-sm transition-colors duration-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-logo-primary ${
                isActive
                  ? "bg-wash font-medium text-text"
                  : "text-ink-2 hover:bg-wash hover:text-text"
              }`}
              onClick={() => onSectionChange(section.id)}
            >
              {isActive && (
                <span className="absolute start-0 top-1/2 h-4 w-[3px] -translate-y-1/2 rounded-full bg-logo-primary" />
              )}
              <span className="relative shrink-0">
                <Icon width={18} height={18} strokeWidth={1.75} />
                {/* A meeting is recording. */}
                {section.id === "meetings" && meetingRecording && (
                  <span className="absolute -end-0.5 -top-0.5 size-2 rounded-full bg-error ring-2 ring-background" />
                )}
              </span>
              <span className="truncate">{t(section.labelKey)}</span>
            </button>
          );
        })}
      </div>
    </nav>
  );
};
