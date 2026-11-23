/**
 * The "lowyap_" wordmark: optically balanced lowercase Bricolage-inspired
 * Vocal Hardware letterforms in the theme's ink, anchored by the Signal
 * Vermilion `_` typing cursor (`var(--color-logo-primary)`).
 */
const HandyTextLogo = ({
  width,
  height,
  className,
}: {
  width?: number;
  height?: number;
  className?: string;
}) => {
  return (
    <svg
      width={width}
      height={height}
      className={className}
      viewBox="0 0 404 108"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      role="img"
      aria-label="lowyap"
    >
      <g
        className="stroke-text"
        strokeWidth="11.5"
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        {/* l: hooked baseline terminal */}
        <path d="M10 10V62C10 67.5 13.5 70 19 70H22" />
        {/* o: optically widened bowl with subtle vertical stress */}
        <rect x="37" y="29" width="40" height="41" rx="20" />
        {/* w: crisp machined twin-valley acoustic chevron */}
        <path d="M95 30L106.5 70L121 38L135.5 70L147 30" />
        {/* y: balanced u-bowl + clean descender shelf */}
        <path d="M167 30V51C167 62 175.5 70 186.5 70C197.5 70 206 62 206 51V30M206 30V79C206 89.5 198 96 187.5 96H177" />
        {/* a: optical bowl + firm vertical stem */}
        <rect x="226" y="29" width="39" height="41" rx="19.5" />
        <path d="M265 29V70" />
        {/* p: full descender stem + optical bowl */}
        <path d="M289 29V96" />
        <rect x="289" y="29" width="39" height="41" rx="19.5" />
      </g>
      {/* Signal Vermilion `_` cursor LED */}
      <rect
        x="346"
        y="63.5"
        width="44"
        height="12"
        rx="4"
        fill="var(--color-logo-primary)"
      />
    </svg>
  );
};

export default HandyTextLogo;
