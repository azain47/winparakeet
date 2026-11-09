import React from "react";

interface InputProps extends React.InputHTMLAttributes<HTMLInputElement> {
  variant?: "default" | "compact";
}

export const Input: React.FC<InputProps> = ({
  className = "",
  variant = "default",
  disabled,
  ...props
}) => {
  const baseClasses =
    "px-2 py-1 text-sm bg-wash border border-line border-b-line-strong rounded-[4px] text-start transition-[background-color,border-color,box-shadow] duration-100 placeholder:text-ink-3";

  // Fluent text box: neutral at rest, an accent underline while focused.
  const interactiveClasses = disabled
    ? "opacity-40 cursor-not-allowed"
    : "hover:bg-line focus:outline-none focus:bg-card focus:border-b-logo-primary focus:shadow-[inset_0_-1px_0_var(--color-logo-primary)]";

  const variantClasses = {
    default: "px-3 py-2",
    compact: "px-2 py-1",
  } as const;

  return (
    <input
      className={`${baseClasses} ${variantClasses[variant]} ${interactiveClasses} ${className}`}
      disabled={disabled}
      {...props}
    />
  );
};
