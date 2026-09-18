import React, { useCallback, useRef } from 'react';
import { CropSide, resizeFromSide } from '../../../../utils/cropSides';

/**
 * BLITZRAW: grips in the middle of each crop edge, for a locked aspect ratio.
 *
 * react-image-crop hides its own edge handles the moment an aspect is set, and
 * it is right to: moving one edge of a locked rectangle cannot leave the other
 * three alone. So it leaves four corners, and a corner is the wrong grip for "a
 * bit wider" on a 3:2 crop that is already framed where you want it.
 *
 * These say what they do instead. The opposite edge stays put and the crop
 * opens evenly about the centre of the other axis. The rule, and every awkward
 * case of it, is in `utils/cropSides.ts`.
 *
 * **Only when a ratio is locked.** With a free crop, react-image-crop's own
 * edge handles are visible and already do the simpler thing, and a second set
 * on the same pixels would just fight them.
 */

/** Matches react-image-crop's own handles, so the set looks like one set. */
const HANDLE_SIZE = 12;

const SIDES: Array<{ id: CropSide; style: React.CSSProperties; cursor: string }> = [
  { id: 'top', style: { top: 0, left: '50%', transform: 'translate(-50%, -50%)' }, cursor: 'ns-resize' },
  { id: 'right', style: { top: '50%', right: 0, transform: 'translate(50%, -50%)' }, cursor: 'ew-resize' },
  { id: 'bottom', style: { bottom: 0, left: '50%', transform: 'translate(-50%, 50%)' }, cursor: 'ns-resize' },
  { id: 'left', style: { top: '50%', left: 0, transform: 'translate(-50%, -50%)' }, cursor: 'ew-resize' },
];

export interface SideHandlesProps {
  /** Width over height. Nothing is drawn without one. */
  aspect: number | null | undefined;
  /** The crop as react-image-crop holds it: percentages of the shown image. */
  crop: { x: number; y: number; width: number; height: number };
  /**
   * The shown image, in the same pixels the crop percentages are of.
   *
   * Partial because the render size is not known until the image has laid out,
   * and the caller has it as an optional pair. Nothing is drawn without both.
   */
  imageSize: { width?: number; height?: number };
  /** Called on every move, with the new crop in percentages. */
  onResize(next: { x: number; y: number; width: number; height: number }): void;
  /** Called once at the end, so the change can be committed and rendered. */
  onResizeEnd(next: { x: number; y: number; width: number; height: number }): void;
}

export default function SideHandles({ aspect, crop, imageSize, onResize, onResizeEnd }: SideHandlesProps) {
  // One shape for the rest of the file, so nothing below has to keep asking
  // whether the image has laid out yet.
  const size =
    imageSize.width && imageSize.height ? { width: imageSize.width, height: imageSize.height } : null;
  // Sized to the crop selection by its parent, so its box is the crop box. Used
  // to turn a pointer position into a position in the image, without having to
  // know where on the page the image itself is.
  const boxRef = useRef<HTMLDivElement | null>(null);

  const drag = useRef<{ pointerId: number; side: CropSide; latest: SideHandlesProps['crop'] } | null>(null);

  /**
   * The pointer, in the pixels the crop is measured in.
   *
   * The scale is worked out from the crop box rather than assumed to be one,
   * because the editor can be zoomed and the whole image scaled with it. A crop
   * with no width would divide by zero, and there is nothing to resize then
   * anyway.
   */
  const toImagePixels = useCallback(
    (clientX: number, clientY: number) => {
      const box = boxRef.current?.getBoundingClientRect();
      if (!box || crop.width <= 0 || crop.height <= 0) {
        return null;
      }
      const cropPxWidth = (crop.width / 100) * (size?.width ?? 0);
      const cropPxHeight = (crop.height / 100) * (size?.height ?? 0);
      if (cropPxWidth <= 0 || cropPxHeight <= 0) {
        return null;
      }

      const scaleX = box.width / cropPxWidth;
      const scaleY = box.height / cropPxHeight;
      if (!(scaleX > 0) || !(scaleY > 0)) {
        return null;
      }

      const imageLeft = box.left - (crop.x / 100) * (size?.width ?? 0) * scaleX;
      const imageTop = box.top - (crop.y / 100) * (size?.height ?? 0) * scaleY;

      return {
        x: (clientX - imageLeft) / scaleX,
        y: (clientY - imageTop) / scaleY,
      };
    },
    [crop.x, crop.y, crop.width, crop.height, size?.width, size?.height],
  );

  const asPercent = useCallback(
    (rect: { x: number; y: number; width: number; height: number }) => ({
      x: (rect.x / (size?.width ?? 1)) * 100,
      y: (rect.y / (size?.height ?? 1)) * 100,
      width: (rect.width / (size?.width ?? 1)) * 100,
      height: (rect.height / (size?.height ?? 1)) * 100,
    }),
    [size?.width, size?.height],
  );

  const handlePointerDown = useCallback((side: CropSide) => (event: React.PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) {
      return;
    }
    // The crop area takes its own drags to move the whole crop. Without this,
    // grabbing an edge would slide the crop instead of resizing it.
    event.preventDefault();
    event.stopPropagation();

    drag.current = { pointerId: event.pointerId, side, latest: crop };
    event.currentTarget.setPointerCapture(event.pointerId);
  }, [crop]);

  const handlePointerMove = useCallback(
    (event: React.PointerEvent<HTMLDivElement>) => {
      const active = drag.current;
      if (!active || active.pointerId !== event.pointerId || !aspect || !size) {
        return;
      }
      const pointer = toImagePixels(event.clientX, event.clientY);
      if (!pointer) {
        return;
      }
      event.preventDefault();
      event.stopPropagation();

      const current = {
        x: (crop.x / 100) * size.width,
        y: (crop.y / 100) * size.height,
        width: (crop.width / 100) * size.width,
        height: (crop.height / 100) * size.height,
      };

      const next = asPercent(resizeFromSide(active.side, current, aspect, pointer, size));
      active.latest = next;
      onResize(next);
    },
    [aspect, crop, size, toImagePixels, asPercent, onResize],
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
      onResizeEnd(active.latest);
    },
    [onResizeEnd],
  );

  if (!aspect || aspect <= 0 || !size) {
    return null;
  }

  return (
    <div ref={boxRef} className="absolute inset-0 pointer-events-none">
      {SIDES.map((side) => (
        <div
          key={side.id}
          className="absolute pointer-events-auto touch-none border border-white/70 bg-black/30"
          style={{
            ...side.style,
            width: HANDLE_SIZE,
            height: HANDLE_SIZE,
            cursor: side.cursor,
            boxSizing: 'border-box',
          }}
          onPointerDown={handlePointerDown(side.id)}
          onPointerMove={handlePointerMove}
          onPointerUp={finish}
          onPointerCancel={finish}
        />
      ))}
    </div>
  );
}
