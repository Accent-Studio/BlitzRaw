/**
 * BLITZRAW: the pen mask's geometry, away from the canvas that draws it.
 *
 * The shape is the same one `src-tauri/src/pen_mask.rs` fills, and the two have
 * to agree or the overlay draws one thing and the mask is another. Both are
 * anchors in the photo's own pixels, each able to carry two absolute handles,
 * and a segment is straight unless one of its two handles is present.
 *
 * Everything here is pure. It is the part worth checking without a canvas, and
 * the part that would otherwise be buried in a three thousand line component.
 */

export interface PenPoint {
  x: number;
  y: number;
}

export interface PenAnchor extends PenPoint {
  /** Shapes the segment arriving at this anchor. Absolute, in photo pixels. */
  handleIn?: PenPoint;
  /** Shapes the segment leaving it. */
  handleOut?: PenPoint;
}

export interface PenParameters {
  points: Array<PenAnchor>;
  closed: boolean;
  isDrawing?: boolean;
  grow?: number;
  feather?: number;
}

/**
 * How close a click has to land on the first anchor to close the path, in
 * screen pixels. Generous, because closing is the common ending and hunting for
 * a four pixel target at the end of every path would be its own small tax.
 */
export const CLOSE_HIT_RADIUS = 12;

/** Below this drag, a press is a corner rather than the start of a curve. */
export const HANDLE_DRAG_THRESHOLD = 4;

/** An area needs three corners. Fewer is a path still being drawn. */
export const MIN_ANCHORS_FOR_AREA = 3;

export function isPenParameters(value: any): value is PenParameters {
  return !!value && Array.isArray(value.points);
}

/** A handle reflected through its anchor, which is what keeps a curve smooth. */
export function mirrorHandle(anchor: PenPoint, handle: PenPoint): PenPoint {
  return { x: 2 * anchor.x - handle.x, y: 2 * anchor.y - handle.y };
}

export function distance(a: PenPoint, b: PenPoint): number {
  return Math.hypot(a.x - b.x, a.y - b.y);
}

/**
 * Moves an anchor and takes its handles with it.
 *
 * Handles are stored absolute, so dragging an anchor without this would leave
 * them where they were and turn a smooth curve into a kink. Illustrator moves
 * them together and so does this.
 */
export function moveAnchor(anchor: PenAnchor, x: number, y: number): PenAnchor {
  const dx = x - anchor.x;
  const dy = y - anchor.y;
  return {
    ...anchor,
    x,
    y,
    handleIn: anchor.handleIn ? { x: anchor.handleIn.x + dx, y: anchor.handleIn.y + dy } : undefined,
    handleOut: anchor.handleOut ? { x: anchor.handleOut.x + dx, y: anchor.handleOut.y + dy } : undefined,
  };
}

/**
 * The anchor a click is on, or -1.
 *
 * `tolerance` is in photo pixels, so the caller divides its screen tolerance by
 * the render scale first. Nearest wins rather than first, or two anchors close
 * together would always hand the click to whichever was drawn earlier.
 */
export function anchorAt(points: Array<PenAnchor>, x: number, y: number, tolerance: number): number {
  let best = -1;
  let bestDist = tolerance;
  for (let i = 0; i < points.length; i++) {
    const d = Math.hypot(points[i].x - x, points[i].y - y);
    if (d <= bestDist) {
      best = i;
      bestDist = d;
    }
  }
  return best;
}

/**
 * Would a click here close the path?
 *
 * Only on the first anchor, and only once there are enough anchors to enclose
 * anything. Closing a two point path would make a line with no area, and the
 * click was much more likely meant as a third point on top of the first.
 */
export function closesPath(
  points: Array<PenAnchor>,
  x: number,
  y: number,
  toleranceInPhotoPixels: number,
): boolean {
  if (points.length < MIN_ANCHORS_FOR_AREA) {
    return false;
  }
  return distance(points[0], { x, y }) <= toleranceInPhotoPixels;
}

/**
 * Draws the path into a 2D context in canvas coordinates.
 *
 * Shared by the Konva overlay and by anything else that needs the outline. The
 * transform is passed in rather than read from anywhere, so the same function
 * serves the full-size canvas and a thumbnail of it.
 *
 * The path is always closed on the way out, matching the fill, which treats an
 * unjoined path as joined because a mask is an area.
 */
export function tracePenPath(
  ctx: {
    beginPath(): void;
    moveTo(x: number, y: number): void;
    lineTo(x: number, y: number): void;
    bezierCurveTo(cp1x: number, cp1y: number, cp2x: number, cp2y: number, x: number, y: number): void;
    closePath(): void;
  },
  points: Array<PenAnchor>,
  toCanvas: (p: PenPoint) => PenPoint,
  closeIt: boolean,
): void {
  if (points.length === 0) {
    return;
  }

  const first = toCanvas(points[0]);
  ctx.beginPath();
  ctx.moveTo(first.x, first.y);

  const lastIndex = closeIt ? points.length : points.length - 1;

  for (let i = 0; i < lastIndex; i++) {
    const a = points[i];
    const b = points[(i + 1) % points.length];
    const pb = toCanvas(b);

    if (!a.handleOut && !b.handleIn) {
      ctx.lineTo(pb.x, pb.y);
      continue;
    }

    const c1 = toCanvas(a.handleOut ?? a);
    const c2 = toCanvas(b.handleIn ?? b);
    ctx.bezierCurveTo(c1.x, c1.y, c2.x, c2.y, pb.x, pb.y);
  }

  if (closeIt) {
    ctx.closePath();
  }
}

/**
 * Where a path stands: what the canvas should let you do to it right now.
 *
 * Placing and editing are genuinely different modes. While placing, a click on
 * the photo adds a point and nothing else may take that click. Once finished,
 * clicks belong to the anchors and the photo underneath goes back to panning.
 */
export function penPhase(parameters: any): 'placing' | 'editing' {
  return parameters?.isDrawing ? 'placing' : 'editing';
}

/* ========================= Editing an existing path =========================
 *
 * Everything below changes a finished path rather than drawing one. They are
 * pure so the gestures in the canvas stay about pointers and modifier keys,
 * and the geometry that has to be right stays somewhere it can be checked.
 */

/** Moves every anchor and every handle by the same amount. */
export function translatePath(points: Array<PenAnchor>, dx: number, dy: number): Array<PenAnchor> {
  return points.map((anchor) => ({
    x: anchor.x + dx,
    y: anchor.y + dy,
    handleIn: anchor.handleIn ? { x: anchor.handleIn.x + dx, y: anchor.handleIn.y + dy } : undefined,
    handleOut: anchor.handleOut ? { x: anchor.handleOut.x + dx, y: anchor.handleOut.y + dy } : undefined,
  }));
}

/**
 * Takes one anchor out and lets the path close over the gap.
 *
 * Nothing else is touched. The two neighbours keep the handles they already
 * had, so the segment that joins them is shaped by the curve each was already
 * carrying and the path keeps its character instead of snapping to a straight
 * line across the hole.
 *
 * A path cannot go below three anchors and still be an area, so the last three
 * refuse to be reduced; the whole mask is the thing to delete at that point.
 */
export function removeAnchor(points: Array<PenAnchor>, index: number): Array<PenAnchor> {
  if (index < 0 || index >= points.length || points.length <= MIN_ANCHORS_FOR_AREA) {
    return points;
  }
  return points.filter((_, i) => i !== index);
}

/**
 * Corner to curve and back, which is Illustrator's convert-point.
 *
 * A corner becomes smooth by growing handles along the line between its two
 * neighbours, which is the direction a curve through it would already be
 * heading, so the shape eases rather than jumps. A third of the way to each
 * neighbour is the length that reads as a gentle curve and is also what most
 * drawing programs use when they do this for you.
 *
 * A curve becomes a corner by simply dropping both handles.
 */
export function toggleAnchorSmooth(points: Array<PenAnchor>, index: number): Array<PenAnchor> {
  const anchor = points[index];
  if (!anchor) {
    return points;
  }

  const next = [...points];

  if (anchor.handleIn || anchor.handleOut) {
    next[index] = { x: anchor.x, y: anchor.y };
    return next;
  }

  const before = points[(index - 1 + points.length) % points.length];
  const after = points[(index + 1) % points.length];

  const dirX = after.x - before.x;
  const dirY = after.y - before.y;
  const length = Math.hypot(dirX, dirY);
  if (length < 1e-6) {
    return points;
  }

  const reach = length / 3;
  const ux = dirX / length;
  const uy = dirY / length;

  next[index] = {
    x: anchor.x,
    y: anchor.y,
    handleIn: { x: anchor.x - ux * reach, y: anchor.y - uy * reach },
    handleOut: { x: anchor.x + ux * reach, y: anchor.y + uy * reach },
  };
  return next;
}

function lerp(a: PenPoint, b: PenPoint, t: number): PenPoint {
  return { x: a.x + (b.x - a.x) * t, y: a.y + (b.y - a.y) * t };
}

/**
 * Puts a new anchor partway along a segment without moving the outline.
 *
 * A cubic cut anywhere is two cubics, and de Casteljau gives both halves
 * exactly: the same points that find the curve at `t` are the control points
 * of the two pieces. So the path through the new anchor is the path that was
 * already there, to the pixel. Splitting by eye instead, which is the obvious
 * shortcut, moves the curve every time and is why adding a point in a lesser
 * editor makes the shape twitch.
 *
 * A straight segment splits at the plain midpoint of its two ends and stays a
 * corner, because a straight run that sprouts handles on a click is a surprise
 * rather than a convenience.
 */
export function splitSegment(points: Array<PenAnchor>, segmentIndex: number, t: number): Array<PenAnchor> {
  const n = points.length;
  if (n < 2 || segmentIndex < 0 || segmentIndex >= n) {
    return points;
  }

  const a = points[segmentIndex];
  const b = points[(segmentIndex + 1) % n];
  const clamped = Math.min(0.999, Math.max(0.001, t));

  const next = [...points];

  if (!a.handleOut && !b.handleIn) {
    const at = lerp(a, b, clamped);
    next.splice(segmentIndex + 1, 0, { x: at.x, y: at.y });
    return next;
  }

  const p0: PenPoint = { x: a.x, y: a.y };
  const p3: PenPoint = { x: b.x, y: b.y };
  const p1 = a.handleOut ?? p0;
  const p2 = b.handleIn ?? p3;

  const q0 = lerp(p0, p1, clamped);
  const q1 = lerp(p1, p2, clamped);
  const q2 = lerp(p2, p3, clamped);
  const r0 = lerp(q0, q1, clamped);
  const r1 = lerp(q1, q2, clamped);
  const split = lerp(r0, r1, clamped);

  next[segmentIndex] = { ...a, handleOut: q0 };
  next[(segmentIndex + 1) % n] = { ...b, handleIn: q2 };
  next.splice(segmentIndex + 1, 0, { x: split.x, y: split.y, handleIn: r0, handleOut: r1 });

  return next;
}

/** One point on a segment, used to walk a segment looking for the nearest. */
function pointOnSegment(a: PenAnchor, b: PenAnchor, t: number): PenPoint {
  if (!a.handleOut && !b.handleIn) {
    return lerp(a, b, t);
  }
  const p1 = a.handleOut ?? { x: a.x, y: a.y };
  const p2 = b.handleIn ?? { x: b.x, y: b.y };
  const mt = 1 - t;
  const c0 = mt * mt * mt;
  const c1 = 3 * mt * mt * t;
  const c2 = 3 * mt * t * t;
  const c3 = t * t * t;
  return {
    x: c0 * a.x + c1 * p1.x + c2 * p2.x + c3 * b.x,
    y: c0 * a.y + c1 * p1.y + c2 * p2.y + c3 * b.y,
  };
}

/** How many places along one segment are tried when looking for a click. */
const HIT_SAMPLES = 24;

/**
 * Which segment a click landed on, and how far along it.
 *
 * Sampled rather than solved. The exact answer for a cubic is a fifth degree
 * root-find, and the click only has to be accurate enough that the anchor
 * appears under the cursor; a twenty-fourth of a segment is well inside the
 * few pixels a hand can aim at anyway.
 *
 * The closing segment is included when the path is closed, so a point can be
 * added to the run between the last anchor and the first like any other.
 */
export function closestSegment(
  points: Array<PenAnchor>,
  closed: boolean,
  x: number,
  y: number,
): { segmentIndex: number; t: number; distance: number } | null {
  const n = points.length;
  if (n < 2) {
    return null;
  }

  const lastSegment = closed ? n : n - 1;
  let best: { segmentIndex: number; t: number; distance: number } | null = null;

  for (let i = 0; i < lastSegment; i++) {
    const a = points[i];
    const b = points[(i + 1) % n];
    for (let step = 0; step <= HIT_SAMPLES; step++) {
      const t = step / HIT_SAMPLES;
      const at = pointOnSegment(a, b, t);
      const d = Math.hypot(at.x - x, at.y - y);
      if (!best || d < best.distance) {
        best = { segmentIndex: i, t, distance: d };
      }
    }
  }

  return best;
}
