import React from "react";

interface SettingsGroupProps {
  title?: string;
  description?: string;
  children: React.ReactNode;
}

/** A titled list of setting rows, each drawn as its own card (Windows Settings style). */
export const SettingsGroup: React.FC<SettingsGroupProps> = ({
  title,
  description,
  children,
}) => {
  return (
    <section className="space-y-2">
      {title && (
        <div className="px-1">
          <h2 className="text-[14px] font-semibold text-text">{title}</h2>
          {description && (
            <p className="mt-0.5 text-xs text-ink-2">{description}</p>
          )}
        </div>
      )}
      <div className="flex flex-col gap-[3px] *:rounded-[6px] *:border *:border-line *:bg-card">
        {children}
      </div>
    </section>
  );
};
