import React, { useCallback, useRef } from 'react';

/**
 * BLITZRAW: a band around the crop box that rotates the photo when dragged.
 *
 * Straightening a horizon with a slider means looking away from the picture to
 * find the slider, moving it, and looking back to see what happened. Taking
 * hold of the picture instead is what every other editor does.
 *
 * **This was four dots on the corners first, and that was wrong twice over.**
 * The corners of a crop are frequently off the top of the canvas, which is
 * clipped, so the grip that was needed was the one that could not be reached.
 * And a dot is a target: it has to be aimed at. A band has no target, so there
 * is nothing to aim at and nothing to draw. The cursor says what will happen,
 * which is the only thing a dot was really for.
 *
 * The photo turns under a crop box that stays where it is, which is what the
 * `rotation` adjustment already does; this is a second way to drive it, not a
 * second thing. The slider and this band write the same value through the same
 * live-preview-then-commit path, so they agree at every moment and either can
 * finish what the other started.
 */

/** Matches the rotation slider, because it is the same number. */
export const MIN_ROTATION = -45;
export const MAX_ROTATION = 45;

/**
 * A clear border left around the crop box before the band starts.
 *
 * react-image-crop's resize handles are 12px squares centred on the edge, so
 * they reach 6px outward. Starting the band on the edge would bury their outer
 * half and make the crop hard to resize, which is a worse trade than a slightly
 * smaller rotate zone.
 */
const EDGE_GAP = 16;

/** How far out from the crop box the band reaches. */
const REACH = 200;

/** From the crop edge to the outside of the band. */
const OUTER = EDGE_GAP + REACH;

/** Holding shift slows the turn down, the same as it does on a slider. */
const FINE_MULTIPLIER = 0.2;

/**
 * A curved double-headed arrow, drawn white on a dark outline so it reads on a
 * bright sky and on a dark foreground alike. A crop edge is exactly where a
 * photo is least predictable, and there is no system cursor for "rotate".
 *
 * Drawn once pointing down and turned for each side, so that the arrows always
 * face the photo: down along the top, up along the bottom, right along the left
 * edge and left along the right. An arrow that points down while you stand to
 * the right of the picture reads as a different gesture entirely.
 */
const rotateCursorSvg = (degrees: number) => `<svg xmlns="http://www.w3.org/2000/svg" width="28" height="28" viewBox="0 0 28 28">
<g transform="rotate(${degrees} 14 14)">
<path d="M6.5 15.5 A8 8 0 0 1 21.5 15.5" fill="none" stroke="#000" stroke-opacity="0.55" stroke-width="5.2" stroke-linecap="round"/>
<path d="M2.5 13.5 L10.5 13.5 L6.5 20.5 Z" fill="#000" fill-opacity="0.55" stroke="#000" stroke-opacity="0.55" stroke-width="2.6" stroke-linejoin="round"/>
<path d="M17.5 13.5 L25.5 13.5 L21.5 20.5 Z" fill="#000" fill-opacity="0.55" stroke="#000" stroke-opacity="0.55" stroke-width="2.6" stroke-linejoin="round"/>
<path d="M6.5 15.5 A8 8 0 0 1 21.5 15.5" fill="none" stroke="#fff" stroke-width="2.2" stroke-linecap="round"/>
<path d="M3.5 14 L9.5 14 L6.5 19.4 Z" fill="#fff"/>
<path d="M18.5 14 L24.5 14 L21.5 19.4 Z" fill="#fff"/>
</g>
</svg>`;

/** Hotspot at the middle of the glyph, so the cursor sits under the pointer. */
const rotateCursor = (degrees: number) =>
  `url("data:image/svg+xml,${encodeURIComponent(rotateCursorSvg(degrees))}") 14 14, grab`;

const clamp = (value: number, low: number, high: number) => Math.max(low, Math.min(high, value));

/**
 * The shortest way round from one angle to another, in degrees.
 *
 * Without this, dragging across the point where `atan2` wraps from +180 to
 * -180 reports a 360 degree jump, and the photo snaps a half turn in one frame.
 */
export function shortestAngleDelta(fromRadians: number, toRadians: number): number {
  const raw = ((toRadians - fromRadians) * 180) / Math.PI;
  return (((raw + 180) % 360) + 360) % 360 - 180;
}

/**
 * Where the drag has got to, given where it started.
 *
 * Pure and exported so the turn can be checked without a pointer: the wrapping
 * and the clamping are the parts worth being sure about.
 */
export function rotationFromDrag(
  startRotation: number,
  startAngle: number,
  currentAngle: number,
  fine: boolean,
): number {
  const delta = shortestAngleDelta(startAngle, currentAngle) * (fine ? FINE_MULTIPLIER : 1);
  return clamp(startRotation + delta, MIN_ROTATION, MAX_ROTATION);
}

/**
 * The band, as four strips rather than one box with a hole in it.
 *
 * A hole is what is wanted: the middle is the crop box, which has to keep
 * taking its own drags to be moved and resized. `pointer-events: none` on a
 * child does not cut a hole in its parent, so the frame is built out of the
 * four pieces that are left when the middle is removed. The top and bottom
 * strips run the full width so the corners belong to them.
 */
const STRIPS: Array<{ id: string; turn: number; style: React.CSSProperties }> = [
  {
    id: 'top',
    turn: 0,
    style: { left: -OUTER, top: -OUTER, width: `calc(100% + ${OUTER * 2}px)`, height: REACH },
  },
  {
    id: 'bottom',
    turn: 180,
    style: { left: -OUTER, bottom: -OUTER, width: `calc(100% + ${OUTER * 2}px)`, height: REACH },
  },
  {
    id: 'left',
    turn: 270,
    style: { left: -OUTER, top: -EDGE_GAP, width: REACH, height: `calc(100% + ${EDGE_GAP * 2}px)` },
  },
  {
    id: 'right',
    turn: 90,
    style: { right: -OUTER, top: -EDGE_GAP, width: REACH, height: `calc(100% + ${EDGE_GAP * 2}px)` },
  },
];

interface RotationHandlesProps {
  /** The rotation showing right now, live value included. */
  rotation: number;
  /** Called once as a drag begins, so the panel can switch to live preview. */
  onRotateStart(): void;
  /** Called on every move with the new angle, for the live preview. */
  onRotate(degrees: number): void;
  /** Called once as the drag ends, with the angle to keep. */
  onRotateEnd(degrees: number): void;
}

export default function RotationHandles({ rotation, onRotateStart, onRotate, onRotateEnd }: RotationHandlesProps) {
  // Sized to the crop selection by its own parent, so its box is the crop box
  // and its centre is what the photo turns about. Read from the DOM rather than
  // computed from the crop numbers, which are percentages of a preview whose
  // size depends on the window.
  const boxRef = useRef<HTMLDivElement | null>(null);

  const drag = useRef<{
    pointerId: number;
    centerX: number;
    centerY: number;
    startAngle: number;
    startRotation: number;
    latest: number;
  } | null>(null);

  const handlePointerDown = useCallback(
    (event: React.PointerEvent<HTMLDivElement>) => {
      if (event.button !== 0) {
        return;
      }
      const box = boxRef.current?.getBoundingClientRect();
      if (!box) {
        return;
      }

      // The crop area is itself a drag target: without this, taking hold of the
      // band could also start drawing a new crop underneath it.
      event.preventDefault();
      event.stopPropagation();

      const centerX = box.left + box.width / 2;
      const centerY = box.top + box.height / 2;

      drag.current = {
        pointerId: event.pointerId,
        centerX,
        centerY,
        startAngle: Math.atan2(event.clientY - centerY, event.clientX - centerX),
        startRotation: rotation,
        latest: rotation,
      };

      event.currentTarget.setPointerCapture(event.pointerId);
      onRotateStart();
    },
    [onRotateStart, rotation],
  );

  const handlePointerMove = useCallback(
    (event: React.PointerEvent<HTMLDivElement>) => {
      const active = drag.current;
      if (!active || active.pointerId !== event.pointerId) {
        return;
      }
      event.preventDefault();
      event.stopPropagation();

      const angle = Math.atan2(event.clientY - active.centerY, event.clientX - active.centerX);
      const next = rotationFromDrag(active.startRotation, active.startAngle, angle, event.shiftKey || event.altKey);

      active.latest = next;
      onRotate(next);
    },
    [onRotate],
  );

  const finish = useCallback(
    (event: React.PointerEvent<HTMLDivElement>) => {
      const active = drag.current;
      if (!active || active.pointerId !== event.pointerId) {
        return;
      }
      event.preventDefault();
      event.stopPropagation();

      drag.current = null;
      if (event.currentTarget.hasPointerCapture(event.pointerId)) {
        event.currentTarget.releasePointerCapture(event.pointerId);
      }
      onRotateEnd(active.latest);
    },
    [onRotateEnd],
  );

  return (
    <div ref={boxRef} className="absolute inset-0 pointer-events-none" aria-hidden="true">
      {STRIPS.map((strip) => (
        <div
          key={strip.id}
          className="absolute pointer-events-auto touch-none"
          style={{ ...strip.style, cursor: rotateCursor(strip.turn) }}
          onPointerDown={handlePointerDown}
          onPointerMove={handlePointerMove}
          onPointerUp={finish}
          onPointerCancel={finish}
        />
      ))}
    </div>
  );
}
