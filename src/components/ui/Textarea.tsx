import React from "react";

interface TextareaProps
  extends React.TextareaHTMLAttributes<HTMLTextAreaElement> {
  variant?: "default" | "compact";
}

export const Textarea: React.FC<TextareaProps> = ({
  className = "",
  variant = "default",
  ...props
}) => {
  const baseClasses =
    "px-2 py-1 text-sm bg-wash border border-line border-b-line-strong rounded-[4px] text-start transition-[background-color,border-color,box-shadow] duration-100 placeholder:text-ink-3 hover:bg-line focus:outline-none focus:bg-card focus:border-b-logo-primary focus:shadow-[inset_0_-1px_0_var(--color-logo-primary)] resize-y";

  const variantClasses = {
    default: "px-3 py-2 min-h-[100px]",
    compact: "px-2 py-1 min-h-[80px]",
  };

  return (
    <textarea
      className={`${baseClasses} ${variantClasses[variant]} ${className}`}
      {...props}
    />
  );
};
