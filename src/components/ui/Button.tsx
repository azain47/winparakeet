import React from "react";

interface ButtonProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  variant?:
    | "primary"
    | "primary-soft"
    | "secondary"
    | "outline"
    | "warning"
    | "danger"
    | "danger-ghost"
    | "ghost";
  size?: "sm" | "md" | "lg";
}

export const Button: React.FC<ButtonProps> = ({
  children,
  className = "",
  variant = "primary",
  size = "md",
  ...props
}) => {
  const baseClasses =
    "font-medium rounded-[4px] border focus:outline-none focus-visible:ring-2 focus-visible:ring-logo-primary focus-visible:ring-offset-1 focus-visible:ring-offset-background transition-[background-color,border-color,filter] duration-100 disabled:opacity-40 disabled:cursor-not-allowed cursor-pointer";

  // Fluent: one accent button per view; everything else is a neutral fill.
  const variantClasses = {
    primary:
      "text-on-accent bg-background-ui border-background-ui hover:brightness-110 active:brightness-95",
    "primary-soft":
      "text-text bg-logo-primary/15 border-transparent hover:bg-logo-primary/25",
    secondary: "text-text bg-wash border-line hover:bg-line",
    // For screens where secondary's grey fill reads as disabled (onboarding)
    outline: "text-text bg-card border-line-strong hover:bg-card-hover",
    // Secondary's neutral resting look, but hover uses the semantic
    // --color-warning token (theme.css) — for buttons sitting on warning
    // surfaces like SecureInputWarning
    warning:
      "text-text bg-wash border-line hover:bg-warning-wash hover:border-warning",
    danger:
      "text-background bg-error border-error hover:brightness-110 active:brightness-95",
    "danger-ghost":
      "text-error border-transparent hover:bg-error-wash focus:bg-error-wash",
    ghost: "text-current border-transparent hover:bg-wash active:bg-line",
  };

  const sizeClasses = {
    sm: "px-2 py-1 text-xs",
    md: "px-4 py-[5px] text-sm",
    lg: "px-4 py-2 text-base",
  };

  return (
    <button
      className={`${baseClasses} ${variantClasses[variant]} ${sizeClasses[size]} ${className}`}
      {...props}
    >
      {children}
    </button>
  );
};
