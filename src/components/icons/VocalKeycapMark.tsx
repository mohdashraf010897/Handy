import React from "react";

interface VocalKeycapMarkProps {
  size?: number | string;
  className?: string;
  /** Render monochrome (e.g. inside tiny badges) or with the Signal Vermilion `_` LED. */
  monochrome?: boolean;
}

/**
 * lowyap Brand Symbol — "Low-Frequency Terrace" (V2, locked permanent baseline).
 *
 * A machined double-bevel hardware keycap housing four asymmetric vocal-cadence
 * terrace bars ("lowkey yap") stepping down onto a glowing Signal Vermilion `_`
 * terminal underscore ("it types"), with a tactile Vermilion homing bar.
 */
export const VocalKeycapMark: React.FC<VocalKeycapMarkProps> = ({
  size = 24,
  className,
  monochrome = false,
}) => (
  <svg
    width={size}
    height={size}
    viewBox="0 0 64 64"
    fill="none"
    xmlns="http://www.w3.org/2000/svg"
    className={className}
    aria-hidden="true"
  >
    {/* Machined outer squircle keycap housing */}
    <rect
      x="3.5"
      y="3.5"
      width="57"
      height="57"
      rx="15"
      fill={monochrome ? "currentColor" : "var(--color-surface)"}
      fillOpacity={monochrome ? 0.12 : 1}
      stroke={monochrome ? "currentColor" : "var(--color-line-strong)"}
      strokeWidth="2.25"
    />
    {/* Recessed spherical key dish bevel + tactile Vermilion homing bar */}
    {!monochrome && (
      <>
        <rect
          x="7.5"
          y="7"
          width="49"
          height="46"
          rx="11.5"
          fill="var(--color-background)"
          stroke="var(--color-line)"
          strokeWidth="1.4"
        />
        <rect
          x="26"
          y="55.5"
          width="12"
          height="2"
          rx="1"
          fill="var(--color-logo-primary)"
          fillOpacity={0.55}
        />
      </>
    )}
    {/* Canonical 64-unit Low-Frequency Terrace legend: 4 vocal bars + Signal Vermilion `_` */}
    <g>
      <rect
        x="13"
        y="25"
        width="5.2"
        height="18"
        rx="2.6"
        fill={monochrome ? "currentColor" : "var(--color-text)"}
        fillOpacity={0.72}
      />
      <rect
        x="21.4"
        y="15.5"
        width="5.2"
        height="27.5"
        rx="2.6"
        fill={monochrome ? "currentColor" : "var(--color-text)"}
      />
      <rect
        x="29.8"
        y="21"
        width="5.2"
        height="22"
        rx="2.6"
        fill={monochrome ? "currentColor" : "var(--color-text)"}
      />
      <rect
        x="38.2"
        y="30.5"
        width="5.2"
        height="12.5"
        rx="2.6"
        fill={monochrome ? "currentColor" : "var(--color-text)"}
        fillOpacity={0.72}
      />
      <rect
        x="38.2"
        y="45.6"
        width="12.8"
        height="4.4"
        rx="2.2"
        fill={monochrome ? "currentColor" : "var(--color-logo-primary)"}
      />
    </g>
  </svg>
);

export default VocalKeycapMark;
