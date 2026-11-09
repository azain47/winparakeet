import React from "react";
import { Tooltip } from "./Tooltip";

interface InfoTooltipProps {
  title?: React.ReactNode;
  text: React.ReactNode;
  placement?: "above" | "below";
}

/** An info icon that explains something in a tooltip. */
export const InfoTooltip: React.FC<InfoTooltipProps> = ({
  title,
  text,
  placement = "above",
}) => (
  <Tooltip title={title} text={text} placement={placement} showOnPress>
    <button
      type="button"
      aria-label="More information"
      className="grid cursor-help text-ink-3 transition-colors duration-150 hover:text-text"
    >
      <svg
        className="size-[15px]"
        fill="none"
        stroke="currentColor"
        viewBox="0 0 24 24"
        aria-hidden="true"
      >
        <path
          strokeLinecap="round"
          strokeLinejoin="round"
          strokeWidth={1.75}
          d="M13 16h-1v-4h-1m1-4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z"
        />
      </svg>
    </button>
  </Tooltip>
);
