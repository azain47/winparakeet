import React from "react";
import ParakeetMascot from "../icons/ParakeetMascot";

interface MascotProps {
  /** Height in px. */
  size: number;
  /** Wave once on arrival, or high-five (re-key it to play again). */
  animation?: "wave" | "slap" | null;
}

// The onboarding layout's 357×400 mascot box, kept so sizes and the wave's pivot match it.
const BOX_ASPECT = 357 / 400;

/** WinParakeet's mascot, optionally animated. */
export const Mascot: React.FC<MascotProps> = ({ size, animation = null }) => (
  <span
    style={{ width: size * BOX_ASPECT, height: size }}
    className={`relative block origin-[50%_90%] rotate-[-8deg] select-none ${animation === "wave" ? "animate-wave" : animation === "slap" ? "animate-slap" : ""}`}
  >
    <ParakeetMascot className="absolute inset-0 h-full w-full" />
  </span>
);
