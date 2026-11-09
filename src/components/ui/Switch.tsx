import React from "react";

interface SwitchProps {
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
  /** Names the switch for assistive tech when no visible label points at it. */
  ariaLabel?: string;
  /** The id of a visible label that names the switch. */
  ariaLabelledBy?: string;
}

/** The on/off switch on its own (Windows 11 toggle); ToggleSwitch puts it in a settings row. */
export const Switch: React.FC<SwitchProps> = ({
  checked,
  onChange,
  disabled = false,
  ariaLabel,
  ariaLabelledBy,
}) => {
  return (
    <label
      className={`group flex items-center ${disabled ? "cursor-not-allowed" : "cursor-pointer"}`}
    >
      <input
        type="checkbox"
        aria-label={ariaLabel}
        aria-labelledby={ariaLabelledBy}
        value=""
        className="sr-only peer"
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
      />
      <span
        className={`relative h-5 w-10 shrink-0 rounded-full border transition-colors duration-150 peer-focus-visible:ring-2 peer-focus-visible:ring-logo-primary peer-focus-visible:ring-offset-2 peer-focus-visible:ring-offset-background peer-disabled:opacity-40 ${
          checked
            ? "border-background-ui bg-background-ui group-hover:brightness-110"
            : "border-ink-2 bg-transparent group-hover:bg-wash"
        }`}
      >
        <span
          className={`absolute top-1/2 size-3 -translate-y-1/2 rounded-full transition-[inset-inline-start,background-color] duration-150 ${
            checked ? "start-[23px] bg-on-accent" : "start-[3px] bg-ink-2"
          }`}
        />
      </span>
    </label>
  );
};
