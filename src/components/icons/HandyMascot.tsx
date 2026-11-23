/**
 * lowyap's tactile hardware emblem for Onboarding & TryIt:
 * an anodized 3D-sculpted Machined Carbon keycap housing the permanent
 * "Low-Frequency Terrace" (V2) Warm Stone vocal bars + Signal Vermilion `_` LED.
 */
const HandyMascot = ({ className = "" }: { className?: string }) => {
  return (
    <svg
      viewBox="0 0 151 165"
      className={className}
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      aria-hidden="true"
    >
      {/* Keycap machined base skirt (tactile travel depth) */}
      <rect
        x="12"
        y="26"
        width="127"
        height="127"
        rx="32"
        fill="#18181B"
        stroke="var(--color-line-strong)"
        strokeWidth="2.5"
      />
      {/* Anodized Carbon keycap body */}
      <rect
        x="12"
        y="16"
        width="127"
        height="125"
        rx="30"
        fill="#222227"
        stroke="#3A3935"
        strokeWidth="2.5"
      />
      {/* Front-skirt Signal Vermilion homing bar */}
      <rect
        x="61"
        y="133"
        width="29"
        height="3.5"
        rx="1.75"
        fill="var(--color-logo-primary)"
        fillOpacity="0.6"
      />
      {/* Spherical dished top surface */}
      <rect
        x="22"
        y="25"
        width="107"
        height="102"
        rx="22"
        fill="#131316"
        stroke="#2E2D29"
        strokeWidth="2"
      />
      {/* Top chamfer catch-light */}
      <path
        d="M34 27H117"
        stroke="#F4F2EC"
        strokeOpacity="0.16"
        strokeWidth="2"
        strokeLinecap="round"
      />
      {/* Low-Frequency Terrace: 4 asymmetric vocal bars + Signal Vermilion `_` */}
      <rect
        x="35"
        y="63"
        width="11.4"
        height="40"
        rx="5.7"
        fill="#F4F2EC"
        fillOpacity="0.72"
      />
      <rect
        x="53.5"
        y="42"
        width="11.4"
        height="61"
        rx="5.7"
        fill="#F4F2EC"
      />
      <rect
        x="72"
        y="54"
        width="11.4"
        height="49"
        rx="5.7"
        fill="#F4F2EC"
      />
      <rect
        x="90.5"
        y="75"
        width="11.4"
        height="28"
        rx="5.7"
        fill="#F4F2EC"
        fillOpacity="0.72"
      />
      <rect
        x="90.5"
        y="109"
        width="26.5"
        height="9.6"
        rx="4.8"
        fill="var(--color-logo-primary)"
      />
    </svg>
  );
};

export default HandyMascot;
