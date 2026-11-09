/** WinParakeet's mascot; the outline and eye follow the theme (`mascot-line`). */
const ParakeetMascot = ({ className = "" }: { className?: string }) => {
  return (
    <svg
      viewBox="8 12 84 86"
      className={className}
      xmlns="http://www.w3.org/2000/svg"
      aria-hidden="true"
    >
      <path
        d="M 82 52 C 84 51 86 49 87 46 C 88 37 83 31 77 30 C 74 22 66 16 57 16 C 46 16 38 24 37 35 C 36 46 33 56 29 66 L 12 94 L 33 80 C 40 80 50 80 58 74 C 68 67 73 58 75 52 C 76 50 77 49 78 48 C 80 48 82 49 82 52 Z"
        fill="#4ADE80"
        className="stroke-mascot-line"
        strokeWidth="2.6"
        strokeLinejoin="round"
      />
      <path
        d="M 44 40 C 54 44 58 56 54 66 C 46 66 38 60 36 52 C 38 47 40 43 44 40 Z"
        fill="#BBF7D0"
        className="stroke-mascot-line"
        strokeWidth="2.2"
        strokeLinejoin="round"
      />
      <path
        d="M 77 30 C 83 31 88 37 87 46 C 86 49 84 51 82 52 C 82 49 80 48 78 48 C 77 42 77 36 77 30 Z"
        fill="#FBBF24"
        className="stroke-mascot-line"
        strokeWidth="2.2"
        strokeLinejoin="round"
      />
      <circle cx="64" cy="31" r="4.2" className="fill-mascot-line" />
      <circle cx="65.2" cy="29.7" r="1.1" fill="#fff" />
    </svg>
  );
};

export default ParakeetMascot;
