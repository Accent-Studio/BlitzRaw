/**
 * BLITZRAW: where a linear gradient goes when you drag one out.
 *
 * # What a linear mask is made of
 *
 * Three numbers, and none of them is what you might expect:
 *
 * - `startX/startY` and `endX/endY` are **two points on one line**. Only the
 *   line matters, not where along it the two points sit: they set its direction
 *   and nothing else. The backend uses them for exactly that.
 * - `range` is the half-width of the fade, measured **across** that line.
 *
 * The backend puts full strength at `range` behind the line, nothing at `range`
 * in front of it, and 50% on the line itself. See `generate_linear_bitmap`.
 *
 * # What was wrong
 *
 * The line was put through the point you clicked, and `range` was set to the
 * whole distance you dragged. So the click landed on the **50% mark**: the
 * effect reached full strength a whole drag-length behind where you started,
 * out in the part of the picture you never dragged over.
 *
 * A gradient should begin where you press. Press for full strength, drag to
 * where it should have faded away to nothing, let go.
 *
 * So the line goes through the **middle** of the drag and the half-width is
 * **half** the drag. Full strength lands exactly on the press, nothing exactly
 * on the release.
 *
 * A radial mask is not like this and is left alone: clicking the middle of an
 * ellipse and dragging its size out is what everyone means by drawing one.
 */

export interface LinearDrag {
  startX: number;
  startY: number;
  endX: number;
  endY: number;
  range: number;
}

/**
 * The gradient a drag from one point to another should make.
 *
 * `handleDist` only decides how far apart the two grab handles are drawn. It
 * has no effect on the picture, because the line they sit on is the same line
 * wherever along it they are.
 */
export function linearFromDrag(
  fromX: number,
  fromY: number,
  toX: number,
  toY: number,
  handleDist: number,
): LinearDrag {
  const dx = toX - fromX;
  const dy = toY - fromY;
  const length = Math.max(1, Math.hypot(dx, dy));

  // Across the drag, which is the direction the line runs in.
  const acrossX = -dy / length;
  const acrossY = dx / length;

  // The line sits halfway along the drag, so the fade reaches full strength at
  // one end of the drag and nothing at the other.
  const midX = fromX + dx / 2;
  const midY = fromY + dy / 2;

  return {
    startX: midX + acrossX * handleDist,
    startY: midY + acrossY * handleDist,
    endX: midX - acrossX * handleDist,
    endY: midY - acrossY * handleDist,
    range: length / 2,
  };
}

/**
 * How strong the mask is at a point, as the backend works it out.
 *
 * Written here only so the rule above can be checked against the sum that
 * actually paints the mask. It mirrors `generate_linear_bitmap` in
 * `mask_generation.rs`; if the two ever disagree, that one is the truth.
 */
export function linearStrengthAt(g: LinearDrag, x: number, y: number): number {
  const lineX = g.endX - g.startX;
  const lineY = g.endY - g.startY;
  const lineLen = Math.hypot(lineX, lineY);
  if (lineLen < 0.1) {
    return 0;
  }
  const perpX = -lineY / lineLen;
  const perpY = lineX / lineLen;
  const alongPerp = (x - g.startX) * perpX + (y - g.startY) * perpY;
  const t = alongPerp / Math.max(g.range, 0.01);
  return Math.min(1, Math.max(0, 0.5 - t * 0.5));
}
