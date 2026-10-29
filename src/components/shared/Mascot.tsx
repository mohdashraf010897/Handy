import React from "react";
import HandyMascot from "../icons/HandyMascot";

interface MascotProps {
  /** Height in px. */
  size: number;
  /** Wiggle once on arrival, or bounce for a high five (re-key it to play again). */
  animation?: "wave" | "slap" | null;
}

// The design's 357×400 image box, kept so sizes and the wiggle's pivot match it.
const BOX_ASPECT = 357 / 400;

/** lowyap's speech-bubble mascot, optionally animated. It pivots on its low tail. */
export const Mascot: React.FC<MascotProps> = ({ size, animation = null }) => (
  <span
    style={{ width: size * BOX_ASPECT, height: size }}
    className={`relative block origin-[18%_85%] rotate-[-8deg] select-none motion-reduce:animate-none ${animation === "wave" ? "animate-wave" : animation === "slap" ? "animate-slap" : ""}`}
  >
    <HandyMascot className="absolute start-[6%] top-[7.25%] h-[85.25%] w-auto" />
  </span>
);
