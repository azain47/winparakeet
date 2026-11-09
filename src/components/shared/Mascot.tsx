import React from "react";
import AppMark from "../icons/AppMark";

interface MascotProps {
  /** Height in px. */
  size: number;
  /** Wave once on arrival, or high-five (re-key it to play again). */
  animation?: "wave" | "slap" | null;
}

/** The app icon as onboarding's guide, optionally animated. */
export const Mascot: React.FC<MascotProps> = ({ size, animation = null }) => (
  <span
    style={{ width: size, height: size }}
    className={`relative block origin-[50%_90%] select-none drop-shadow-[0_6px_14px_rgba(0,0,0,0.22)] ${animation === "wave" ? "animate-wave" : animation === "slap" ? "animate-slap" : ""}`}
  >
    <AppMark className="absolute inset-0 h-full w-full" />
  </span>
);
