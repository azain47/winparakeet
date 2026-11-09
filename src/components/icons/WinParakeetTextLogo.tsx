// The product name is a brand, not translatable text.
const NAME = "WinParakeet";

const WinParakeetTextLogo = ({
  width,
  height,
  className,
}: {
  width?: number;
  height?: number;
  className?: string;
}) => {
  // textLength pins the wordmark's width whatever font the system picks; the
  // back copy draws the outline (logo-stroke), the front copy the fill. The
  // outline width is inline: .logo-stroke's own stroke-width would win over
  // an SVG attribute.
  const text = {
    x: 500,
    y: 165,
    textAnchor: "middle",
    textLength: 960,
    lengthAdjust: "spacingAndGlyphs",
    fontSize: 180,
    fontWeight: 800,
  } as const;
  return (
    <svg
      width={width}
      height={height}
      className={className}
      viewBox="0 0 1000 220"
      xmlns="http://www.w3.org/2000/svg"
      role="img"
      aria-label={NAME}
    >
      <text
        {...text}
        className="logo-stroke"
        style={{ strokeWidth: 16, strokeLinejoin: "round" }}
      >
        {NAME}
      </text>
      <text {...text} className="logo-primary">
        {NAME}
      </text>
    </svg>
  );
};

export default WinParakeetTextLogo;
