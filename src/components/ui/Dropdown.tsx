import React, { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

/** The menu's tallest (`max-h-60`), and a row's usual height, in rem. */
const MENU_MAX_REM = 15;
const ROW_REM = 2.25;

export interface DropdownOption {
  value: string;
  label: React.ReactNode;
  description?: string;
  disabled?: boolean;
}

interface DropdownProps {
  options: DropdownOption[];
  className?: string;
  menuClassName?: string;
  selectedValue: string | null;
  onSelect: (value: string) => void;
  placeholder?: string;
  disabled?: boolean;
  onOpen?: () => void;
  /** Where the menu opens: below the button (default), or above it. */
  placement?: "below" | "above";
}

/**
 * Whether a menu of `rows` fits better above `trigger`: too little room
 * below it in its scroll area (which would clip the menu) or the window, and
 * more room above.
 */
const opensBetterAbove = (trigger: HTMLElement, rows: number): boolean => {
  const rect = trigger.getBoundingClientRect();
  let top = 0;
  let bottom = window.innerHeight;
  for (let el = trigger.parentElement; el; el = el.parentElement) {
    const overflow = getComputedStyle(el).overflowY;
    if (overflow === "auto" || overflow === "scroll") {
      const area = el.getBoundingClientRect();
      top = Math.max(area.top, 0);
      bottom = Math.min(area.bottom, window.innerHeight);
      break;
    }
  }
  const rem = parseFloat(getComputedStyle(document.documentElement).fontSize);
  const menu = Math.min(rows * ROW_REM + 0.5, MENU_MAX_REM) * rem;
  const below = bottom - rect.bottom;
  const above = rect.top - top;
  return below < menu + 8 && above > below;
};

export const Dropdown: React.FC<DropdownProps> = ({
  options,
  selectedValue,
  onSelect,
  className = "",
  menuClassName,
  placeholder = "Select an option...",
  disabled = false,
  onOpen,
  placement = "below",
}) => {
  const { t } = useTranslation();
  const [isOpen, setIsOpen] = useState(false);
  const [flipped, setFlipped] = useState(false);
  const dropdownRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    const handleClickOutside = (event: MouseEvent) => {
      if (
        dropdownRef.current &&
        !dropdownRef.current.contains(event.target as Node)
      ) {
        setIsOpen(false);
      }
    };
    document.addEventListener("mousedown", handleClickOutside);
    return () => document.removeEventListener("mousedown", handleClickOutside);
  }, []);

  const selectedOption = options.find(
    (option) => option.value === selectedValue,
  );

  const handleSelect = (value: string) => {
    onSelect(value);
    setIsOpen(false);
  };

  const handleToggle = () => {
    if (disabled) return;
    if (!isOpen) {
      onOpen?.();
      setFlipped(
        placement === "below" &&
          triggerRef.current !== null &&
          opensBetterAbove(triggerRef.current, Math.max(options.length, 1)),
      );
    }
    setIsOpen(!isOpen);
  };

  const handleKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    if (event.key !== "Escape" || !isOpen || disabled) return;
    event.preventDefault();
    event.stopPropagation();
    setIsOpen(false);
    triggerRef.current?.focus();
  };

  return (
    <div
      className={`relative ${className}`}
      ref={dropdownRef}
      onKeyDown={handleKeyDown}
    >
      <button
        type="button"
        ref={triggerRef}
        className={`px-3 py-[5px] text-sm bg-wash border border-line rounded-[4px] min-w-[200px] w-full text-start grid grid-cols-[1fr_auto] gap-2 items-center transition-colors duration-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-logo-primary ${
          disabled
            ? "opacity-40 cursor-not-allowed"
            : "cursor-pointer hover:bg-line"
        }`}
        onClick={handleToggle}
        disabled={disabled}
        aria-expanded={isOpen}
      >
        <span className="truncate">{selectedOption?.label || placeholder}</span>
        <svg
          className="size-3.5 text-ink-2"
          fill="none"
          stroke="currentColor"
          viewBox="0 0 24 24"
          aria-hidden="true"
        >
          <path
            strokeLinecap="round"
            strokeLinejoin="round"
            strokeWidth={1.75}
            d="M19 9l-7 7-7-7"
          />
        </svg>
      </button>
      {isOpen && !disabled && (
        <div
          className={`absolute ${placement === "above" || flipped ? "bottom-full mb-1" : "top-full mt-1"} animate-drop bg-flyout border border-line rounded-[8px] shadow-flyout z-50 max-h-60 overflow-y-auto p-1 ${
            menuClassName ?? "left-0 right-0"
          }`}
        >
          {options.length === 0 ? (
            <div className="px-2 py-1.5 text-sm text-ink-3">
              {t("common.noOptionsFound")}
            </div>
          ) : (
            options.map((option) => {
              const isChosen = selectedValue === option.value;
              return (
                <button
                  key={option.value}
                  type="button"
                  className={`relative flex w-full items-center gap-2 rounded-[4px] px-3 text-sm text-start transition-colors duration-100 ${
                    option.description ? "py-2" : "py-1.5"
                  } ${isChosen ? "bg-wash" : "hover:bg-wash"} ${
                    option.disabled ? "opacity-40 cursor-not-allowed" : ""
                  }`}
                  onClick={() => handleSelect(option.value)}
                  disabled={option.disabled}
                >
                  {isChosen && (
                    <span className="absolute start-0 top-1/2 h-4 w-[3px] -translate-y-1/2 rounded-full bg-logo-primary" />
                  )}
                  <span className="min-w-0 flex-1">
                    <span className="block whitespace-normal break-words">
                      {option.label}
                    </span>
                    {option.description && (
                      <span className="mt-0.5 block whitespace-normal text-xs font-normal leading-snug text-ink-2">
                        {option.description}
                      </span>
                    )}
                  </span>
                </button>
              );
            })
          )}
        </div>
      )}
    </div>
  );
};
