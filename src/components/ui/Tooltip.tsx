import React, {
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";
import { prefersReducedMotion } from "@/lib/utils/motion";

interface TooltipProps {
  /** A name over the line, for an icon with no words of its own. */
  title?: React.ReactNode;
  text: React.ReactNode;
  /** One element; it's described by the tooltip while the tooltip shows. */
  children: React.ReactElement<{ "aria-describedby"?: string }>;
  /** Where it goes when there's room; it flips when there isn't. */
  placement?: "above" | "below";
  /** Pressing shows it at once, for an element that only explains; otherwise a press hides it. */
  showOnPress?: boolean;
  className?: string;
}

const DELAY = 380;
const EDGE = 8;
const GAP = 8;

// Once a tooltip has shown, the next one under the pointer shows at once, so
// running along a row of icons reads each without waiting again.
let lastHidden = 0;

/** Shows after a delay on hover, immediately on keyboard focus; flips sides when there's no room. */
export const Tooltip: React.FC<TooltipProps> = ({
  title,
  text,
  children,
  placement = "above",
  showOnPress = false,
  className = "",
}) => {
  const id = useId();
  const anchorRef = useRef<HTMLSpanElement>(null);
  const tipRef = useRef<HTMLDivElement>(null);
  const timer = useRef<number>(0);
  const [open, setOpen] = useState(false);
  const [place, setPlace] = useState<{
    top: number;
    left: number;
  } | null>(null);

  const show = (now = false) => {
    window.clearTimeout(timer.current);
    if (now || Date.now() - lastHidden < 600) setOpen(true);
    else timer.current = window.setTimeout(() => setOpen(true), DELAY);
  };
  const hide = () => {
    window.clearTimeout(timer.current);
    setOpen((was) => {
      if (was) lastHidden = Date.now();
      return false;
    });
    setPlace(null);
  };

  const handleKeyDown = (event: React.KeyboardEvent<HTMLSpanElement>) => {
    if (
      event.key !== "Escape" ||
      !open ||
      event.defaultPrevented ||
      event.nativeEvent.isComposing ||
      event.nativeEvent.keyCode === 229
    )
      return;
    event.preventDefault();
    event.stopPropagation();
    hide();
  };

  useEffect(() => () => window.clearTimeout(timer.current), []);

  // Anything scrolling moves the element out from under its tooltip.
  useEffect(() => {
    if (!open) return;
    window.addEventListener("scroll", hide, true);
    return () => window.removeEventListener("scroll", hide, true);
  }, [open]);

  useLayoutEffect(() => {
    const anchor = anchorRef.current;
    const tip = tipRef.current;
    if (!open || !anchor || !tip) return;
    const a = anchor.getBoundingClientRect();
    const w = tip.offsetWidth;
    const h = tip.offsetHeight;
    const roomAbove = a.top - GAP - h >= EDGE;
    const roomBelow = a.bottom + GAP + h <= window.innerHeight - EDGE;
    const below = placement === "below" ? roomBelow || !roomAbove : !roomAbove;
    const center = a.left + a.width / 2;
    const left = Math.min(
      Math.max(center - w / 2, EDGE),
      window.innerWidth - w - EDGE,
    );
    setPlace({
      top: below ? a.bottom + GAP : a.top - GAP - h,
      left,
    });
    if (!prefersReducedMotion())
      tip.animate(
        [
          { opacity: 0, translate: `0 ${below ? -3 : 3}px` },
          { opacity: 1, translate: "0 0" },
        ],
        { duration: 160, easing: "cubic-bezier(0.22, 1, 0.36, 1)" },
      );
  }, [open, placement]);

  return (
    <span
      ref={anchorRef}
      className={`inline-grid ${className}`}
      onPointerEnter={(e) => e.pointerType === "mouse" && show()}
      onPointerLeave={hide}
      onPointerDown={() => (showOnPress ? show(true) : hide())}
      onFocus={(e) => e.target.matches(":focus-visible") && show(true)}
      onBlur={hide}
      onKeyDown={handleKeyDown}
    >
      {React.cloneElement(children, {
        "aria-describedby": open ? id : undefined,
      })}
      {open &&
        createPortal(
          <div
            ref={tipRef}
            id={id}
            role="tooltip"
            style={{
              top: place?.top ?? -9999,
              left: place?.left ?? -9999,
              visibility: place ? "visible" : "hidden",
            }}
            className="pointer-events-none fixed z-[9999] w-max max-w-[260px] rounded-[4px] border border-line bg-flyout px-[9px] py-[6px] text-start font-ui text-label leading-[17px] text-text shadow-flyout"
          >
            {title && <b className="block font-semibold">{title}</b>}
            {/* Block, not inline: WebKit applies a grid's margin in an inline span only
                after the entrance animation, so the tooltip grows onto its element. */}
            <span className={`block ${title ? "text-ink-2" : ""}`}>{text}</span>
          </div>,
          document.body,
        )}
    </span>
  );
};
