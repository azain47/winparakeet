import React from "react";

/** A settings page's title, Windows Settings style. */
export const PageTitle: React.FC<{
  children: React.ReactNode;
  className?: string;
}> = ({ children, className = "" }) => (
  <h1
    className={`font-display text-[26px] font-semibold leading-tight tracking-[-0.01em] ${className}`}
  >
    {children}
  </h1>
);
