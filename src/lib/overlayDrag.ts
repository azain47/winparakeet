import type React from "react";
import { useRef } from "react";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { commands } from "@/bindings";

// How far a press has to move before it's a drag, so a click on the bar
// (dismissing a notice) still clicks.
const DRAG_THRESHOLD_PX = 4;

/**
 * Lets the user drag a bar's window around the screen. Spread the handlers on
 * the bar's card. A press on a button stays a click; any other press that
 * moves drags, and the click that would end it is swallowed. The backend moves
 * the window and saves where it's dropped (overlay.rs, "dragging").
 */
export function useOverlayDrag() {
  const drag = useRef<{
    pointerId: number;
    x: number;
    y: number;
    started: Promise<unknown> | null;
    frame: number | null;
  } | null>(null);
  const swallowClick = useRef(false);

  const finish = (element: HTMLElement) => {
    const current = drag.current;
    drag.current = null;
    document.documentElement.classList.remove("ov-dragging");
    if (!current) return;
    if (element.hasPointerCapture(current.pointerId)) {
      element.releasePointerCapture(current.pointerId);
    }
    if (current.frame !== null) cancelAnimationFrame(current.frame);
    if (current.started) {
      swallowClick.current = true;
      setTimeout(() => (swallowClick.current = false), 0);
      void current.started
        .then(() => commands.endOverlayDrag())
        .catch(console.error);
    }
  };

  const onPointerDown = (event: React.PointerEvent<HTMLElement>) => {
    if (event.button !== 0) return;
    if ((event.target as HTMLElement).closest("button")) return;
    event.currentTarget.setPointerCapture(event.pointerId);
    drag.current = {
      pointerId: event.pointerId,
      x: event.screenX,
      y: event.screenY,
      started: null,
      frame: null,
    };
  };

  const onPointerMove = (event: React.PointerEvent<HTMLElement>) => {
    const current = drag.current;
    if (!current || current.pointerId !== event.pointerId) return;
    if (!current.started) {
      const moved = Math.hypot(
        event.screenX - current.x,
        event.screenY - current.y,
      );
      if (moved < DRAG_THRESHOLD_PX) return;
      document.documentElement.classList.add("ov-dragging");
      current.started = commands
        .startOverlayDrag(getCurrentWebviewWindow().label)
        .catch(console.error);
    }
    // One move per frame; the backend reads the cursor itself.
    if (current.frame !== null) return;
    current.frame = requestAnimationFrame(() => {
      current.frame = null;
      void current.started
        ?.then(() => commands.moveOverlayDrag())
        .catch(console.error);
    });
  };

  const onPointerEnd = (event: React.PointerEvent<HTMLElement>) => {
    if (drag.current?.pointerId !== event.pointerId) return;
    finish(event.currentTarget);
  };

  const onClickCapture = (event: React.MouseEvent<HTMLElement>) => {
    if (!swallowClick.current) return;
    swallowClick.current = false;
    event.stopPropagation();
    event.preventDefault();
  };

  return {
    onPointerDown,
    onPointerMove,
    onPointerUp: onPointerEnd,
    onPointerCancel: onPointerEnd,
    onLostPointerCapture: onPointerEnd,
    onClickCapture,
  };
}
