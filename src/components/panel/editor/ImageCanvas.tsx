import { useState, useEffect, useRef, useCallback, memo, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { KELVIN_STEP } from '../../adjustments/Color';
import ReactCrop from 'react-image-crop';
import 'react-image-crop/dist/ReactCrop.css';
import { Stage, Layer, Ellipse, Line, Transformer, Group, Circle, Rect, Shape } from 'react-konva';
import { PercentCrop, Crop } from 'react-image-crop';
import { Stamp, Bandage } from 'lucide-react';
import { Adjustments, AiPatch, Coord, MaskContainer } from '../../../utils/adjustments';
import { Mask, SubMask, SubMaskMode, ToolType } from '../right/Masks';
import { AppSettings, BrushSettings, SelectedImage } from '../../ui/AppProperties';
import { RenderSize } from '../../../hooks/useImageRenderSize';
import { useOsPlatform } from '../../../hooks/useOsPlatform';
import { useTranslation } from 'react-i18next';
import type { OverlayMode } from '../right/CropPanel';
import CompositionOverlays from './overlays/CompositionOverlays';
import RotationHandles from './overlays/RotationHandles';
import SideHandles from './overlays/SideHandles';
import { useEditorStore } from '../../../store/useEditorStore';
import { dismissWbPicker } from '../../../utils/wbPicker';
import { linearFromDrag } from '../../../utils/linearMask';
import {
  CLOSE_HIT_RADIUS,
  HANDLE_DRAG_THRESHOLD,
  MIN_ANCHORS_FOR_AREA,
  PenAnchor,
  closesPath,
  closestSegment,
  mirrorHandle,
  moveAnchor,
  removeAnchor,
  splitSegment,
  toggleAnchorSmooth,
  tracePenPath,
  translatePath,
} from '../../../utils/penMask';

interface CursorPreview {
  visible: boolean;
  x: number;
  y: number;
}

interface DrawnLine {
  brushSize: number;
  feather?: number;
  flow?: number;
  points: Array<Coord>;
  tool: ToolType;
}

interface ImageCanvasProps {
  appSettings: AppSettings | null;
  activeAiPatchContainerId: string | null;
  activeAiSubMaskId: string | null;
  activeMaskContainerId: string | null;
  activeMaskId: string | null;
  adjustments: Adjustments;
  brushSettings: BrushSettings | null;
  crop: Crop | null;
  finalPreviewUrl: string | null;
  /** BLITZRAW: the disk preview, shown while the decode runs. */
  cachedPreviewUrl?: string | null;
  handleCropComplete(c: Crop, cp: PercentCrop): void;
  imageRenderSize: RenderSize;
  isAiEditing: boolean;
  isCropping: boolean;
  isMaskControlHovered: boolean;
  /** BLITZRAW: whether the red is drawn. The rule is in utils/maskOverlay.ts. */
  isMaskOverlayWanted: boolean;
  isMasking: boolean;
  isSliderDragging: boolean;
  isStraightenActive: boolean;
  isRotationActive?: boolean;
  maskOverlayUrl: string | null;
  onGenerateAiMask(id: string | null, start: Coord, end: Coord): void;
  onLiveMaskPreview?: (previewMaskDef: any) => void;
  onManualCleanup?(subMaskId: string, sourceX: number, sourceY: number): Promise<void> | void;
  onQuickErase(subMaskId: string | null, startPoint: Coord, endpoint: Coord): void;
  onSelectAiSubMask(id: string | null): void;
  onSelectMask(id: string | null): void;
  onSelectAiPatchContainer?: (id: string | null) => void;
  onSelectMaskContainer?: (id: string | null) => void;
  onStraighten(val: number): void;
  selectedImage: SelectedImage;
  setCrop(crop: Crop, perfentCrop: PercentCrop): void;
  setIsMaskHovered(isHovered: boolean): void;
  setIsMaskTouchInteracting(isInteracting: boolean): void;
  showOriginal: boolean;
  transformedOriginalUrl: string | null;
  uncroppedAdjustedPreviewUrl: string | null;
  updateSubMask(id: string | null, subMask: Partial<SubMask>): void;
  interactivePatch?: { url: string; normX: number; normY: number; normW: number; normH: number } | null;
  isWbPickerActive?: boolean;
  onWbPicked?: () => void;
  setAdjustments(fn: (prev: Adjustments) => Adjustments): void;
  overlayMode?: OverlayMode;
  overlayRotation?: number;
  cursorStyle: string;
  isMaxZoom?: boolean;
  liveRotation?: number | null;
  transformState: { scale: number; positionX: number; positionY: number };
  hasRenderedFirstFrame: boolean;
}

interface MaskOverlayProps {
  adjustments: Adjustments;
  imageHeight: number;
  imageWidth: number;
  onMaskInteractionEnd(): void;
  onMaskInteractionStart(event?: any): void;
  isToolActive: boolean;
  isSelected: boolean;
  showBrushStrokes?: boolean;
  onMaskMouseEnter(): void;
  onMaskMouseLeave(): void;
  onPreviewUpdate?(id: string, subMask: Partial<SubMask>): void;
  onSelect(): void;
  onUpdate(id: string, subMask: Partial<SubMask>): void;
  scale: number;
  subMask: SubMask;
  offsetX: number;
  offsetY: number;
  stageScale: number;
  /** BLITZRAW: pen only. Where the cursor is, in canvas pixels, so the path can
      trail a rubber band to it while points are still being placed. */
  penCursor?: Coord | null;
  /** BLITZRAW: pen only. The anchor whose handles are being pulled out, so its
      handles stay on screen while the rest of the path's stay hidden. */
  penActiveAnchor?: number;
  /** BLITZRAW: makes a copy of this sub-mask beside the original and selects it.
      The overlay can change its own sub-mask but cannot make another one, so
      the canvas, which holds the whole adjustment, does it. */
  onDuplicateSubMask?(subMask: SubMask): void;
}

const getEdgeFadeStyle = (fadeDistancePx: number = 128): React.CSSProperties => ({
  WebkitMaskImage: `
    linear-gradient(to right, transparent, black ${fadeDistancePx}px, black calc(100% - ${fadeDistancePx}px), transparent),
    linear-gradient(to bottom, transparent, black ${fadeDistancePx}px, black calc(100% - ${fadeDistancePx}px), transparent)
  `,
  WebkitMaskComposite: 'source-in',
  maskImage: `
    linear-gradient(to right, transparent, black ${fadeDistancePx}px, black calc(100% - ${fadeDistancePx}px), transparent),
    linear-gradient(to bottom, transparent, black ${fadeDistancePx}px, black calc(100% - ${fadeDistancePx}px), transparent)
  `,
  maskComposite: 'intersect',
});

const OptimizedBrushLine = memo(
  ({ line, scale, cropX, cropY }: { line: DrawnLine; scale: number; cropX: number; cropY: number }) => {
    const flattenedPoints = useMemo(() => {
      const pts = new Float32Array(line.points.length * 2);
      for (let i = 0; i < line.points.length; i++) {
        pts[i * 2] = (line.points[i].x - cropX) * scale;
        pts[i * 2 + 1] = (line.points[i].y - cropY) * scale;
      }
      return Array.from(pts);
    }, [line.points, scale, cropX, cropY]);

    return (
      <Line
        hitStrokeWidth={line.brushSize * scale}
        lineCap="round"
        lineJoin="round"
        points={flattenedPoints}
        stroke="transparent"
        strokeScaleEnabled={false}
        perfectDrawEnabled={false}
        shadowForStrokeEnabled={false}
      />
    );
  },
);

const SourcePreviewLine = memo(
  ({
    line,
    scale,
    cropX,
    cropY,
    dx,
    dy,
  }: {
    line: DrawnLine;
    scale: number;
    cropX: number;
    cropY: number;
    dx: number;
    dy: number;
  }) => {
    const flattenedPoints = useMemo(() => {
      const pts = new Float32Array(line.points.length * 2);
      for (let i = 0; i < line.points.length; i++) {
        pts[i * 2] = (line.points[i].x + dx - cropX) * scale;
        pts[i * 2 + 1] = (line.points[i].y + dy - cropY) * scale;
      }
      return Array.from(pts);
    }, [line.points, scale, cropX, cropY, dx, dy]);

    return (
      <Group>
        <Line
          lineCap="round"
          lineJoin="round"
          points={flattenedPoints}
          stroke="rgba(255, 255, 255, 0.15)"
          strokeWidth={line.brushSize * scale}
          strokeScaleEnabled={false}
          perfectDrawEnabled={false}
          shadowForStrokeEnabled={false}
        />
        <Line
          lineCap="round"
          lineJoin="round"
          points={flattenedPoints}
          stroke="white"
          strokeWidth={1.5}
          dash={[4, 4]}
          opacity={0.8}
          strokeScaleEnabled={false}
          perfectDrawEnabled={false}
          shadowForStrokeEnabled={false}
        />
      </Group>
    );
  },
);

const MaskOverlay = memo(
  ({
    adjustments,
    imageHeight,
    imageWidth,
    onMaskInteractionEnd,
    onMaskInteractionStart,
    isToolActive,
    isSelected,
    showBrushStrokes = true,
    onMaskMouseEnter,
    onMaskMouseLeave,
    onPreviewUpdate,
    onSelect,
    onUpdate,
    scale,
    subMask,
    offsetX,
    offsetY,
    stageScale,
    penCursor = null,
    penActiveAnchor = -1,
    onDuplicateSubMask,
  }: MaskOverlayProps) => {
    const shapeRef = useRef<any>(null);
    const trRef = useRef<any>(null);
    const rotateStartRef = useRef<any>(null);

    const crop = adjustments.crop;
    const isPercent = crop?.unit === '%';
    const cropX = crop ? (isPercent ? (crop.x / 100) * imageWidth : crop.x) : 0;
    const cropY = crop ? (isPercent ? (crop.y / 100) * imageHeight : crop.y) : 0;
    const cropW = crop ? (isPercent ? (crop.width / 100) * imageWidth : crop.width) : imageWidth;
    const cropH = crop ? (isPercent ? (crop.height / 100) * imageHeight : crop.height) : imageHeight;

    const [p, setP] = useState(subMask.parameters);
    const pRef = useRef(p);
    const isDragging = useRef(false);

    const dragStartPointer = useRef<Coord | null>(null);
    const dragStartParams = useRef<any>(null);

    const getPointer = useCallback(
      (stage: any) => {
        const pos = stage.getPointerPosition();
        if (!pos) return null;
        return { x: pos.x / stageScale - offsetX, y: pos.y / stageScale - offsetY };
      },
      [offsetX, offsetY, stageScale],
    );

    useEffect(() => {
      if (!isDragging.current) {
        setP(subMask.parameters);
        pRef.current = subMask.parameters;
      }
    }, [subMask.parameters]);

    const updateP = useCallback((newP: any) => {
      setP(newP);
      pRef.current = newP;
    }, []);

    const handleMaskTouchStart = useCallback(
      (e: any) => {
        if (e.evt && typeof e.evt.button === 'number' && e.evt.button !== 0) return;

        onMaskInteractionStart(e);
        if (e.evt.cancelable) e.evt.preventDefault();
        e.evt.stopPropagation?.();
      },
      [onMaskInteractionStart],
    );

    const handleMaskTouchEnd = useCallback(() => {
      onMaskInteractionEnd();
    }, [onMaskInteractionEnd]);

    const handleSelect = isToolActive ? undefined : onSelect;

    useEffect(() => {
      if (isSelected && trRef.current && shapeRef.current) {
        trRef.current?.nodes([shapeRef.current]);
        trRef.current?.getLayer().batchDraw();
      }
    }, [isSelected, isToolActive]);

    const lockDragBoundFunc = useCallback(function (this: any) {
      return this.getAbsolutePosition();
    }, []);

    const handleRadialDragStart = useCallback(
      (e: any) => {
        if (e.evt && typeof e.evt.button === 'number' && e.evt.button !== 0) return;
        isDragging.current = true;
        onMaskInteractionStart(e);
        dragStartPointer.current = getPointer(e.target.getStage());
        dragStartParams.current = { ...pRef.current };
      },
      [onMaskInteractionStart, getPointer],
    );

    const handleRadialDragMove = useCallback(
      (e: any) => {
        const pointerPos = getPointer(e.target.getStage());
        if (!pointerPos || !dragStartPointer.current || !dragStartParams.current) return;

        const dx = (pointerPos.x - dragStartPointer.current.x) / scale;
        const dy = (pointerPos.y - dragStartPointer.current.y) / scale;

        const newP = {
          ...dragStartParams.current,
          centerX: dragStartParams.current.centerX + dx,
          centerY: dragStartParams.current.centerY + dy,
        };

        updateP(newP);
        if (onPreviewUpdate) onPreviewUpdate(subMask.id, { parameters: newP });

        onUpdate(subMask.id, { parameters: newP });
      },
      [scale, updateP, onPreviewUpdate, subMask.id, getPointer, onUpdate],
    );

    const handleRadialDragEnd = useCallback(() => {
      isDragging.current = false;
      onMaskInteractionEnd();
      onUpdate(subMask.id, { parameters: pRef.current });
    }, [subMask.id, onMaskInteractionEnd, onUpdate]);

    const handleRadialTransformStart = useCallback(
      (e: any) => {
        isDragging.current = true;
        onMaskInteractionStart(e);
      },
      [onMaskInteractionStart],
    );

    const handleRadialTransform = useCallback(() => {
      const node = shapeRef.current;
      if (!node) return;

      const scaleX = Math.abs(node.scaleX());
      const scaleY = Math.abs(node.scaleY());

      if (pRef.current.radiusX * scaleX < 5 || pRef.current.radiusY * scaleY < 5) {
        node.scaleX(node.lastValidScaleX || 1);
        node.scaleY(node.lastValidScaleY || 1);
      } else {
        node.lastValidScaleX = scaleX;
        node.lastValidScaleY = scaleY;
      }

      const newRadiusX = pRef.current.radiusX * node.scaleX();
      const newRadiusY = pRef.current.radiusY * node.scaleY();

      const newP = {
        ...pRef.current,
        centerX: node.x() / scale + cropX,
        centerY: node.y() / scale + cropY,
        radiusX: newRadiusX,
        radiusY: newRadiusY,
        rotation: node.rotation(),
      };

      if (onPreviewUpdate) {
        onPreviewUpdate(subMask.id, { parameters: newP });
      }

      onUpdate(subMask.id, { parameters: newP });
    }, [onPreviewUpdate, scale, cropX, cropY, subMask.id, onUpdate]);

    const handleRadialTransformEnd = useCallback(() => {
      const node = shapeRef.current;
      if (!node) return;

      const scaleX = node.scaleX();
      const scaleY = node.scaleY();

      const newRadiusX = pRef.current.radiusX * scaleX;
      const newRadiusY = pRef.current.radiusY * scaleY;

      node.scaleX(1);
      node.scaleY(1);

      const newP = {
        ...pRef.current,
        centerX: node.x() / scale + cropX,
        centerY: node.y() / scale + cropY,
        radiusX: newRadiusX,
        radiusY: newRadiusY,
        rotation: node.rotation(),
      };

      updateP(newP);
      isDragging.current = false;
      onMaskInteractionEnd();
      onUpdate(subMask.id, { parameters: newP });
    }, [scale, cropX, cropY, updateP, onMaskInteractionEnd, onUpdate, subMask.id]);

    const setRotateCursor = useCallback(
      (stage: any, pointerPos: any) => {
        const cx = (pRef.current.centerX - cropX) * scale;
        const cy = (pRef.current.centerY - cropY) * scale;
        const angle = Math.atan2(pointerPos.y - cy, pointerPos.x - cx) * (180 / Math.PI);

        const svgStr = `<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32" viewBox="0 0 32 32" fill="none" stroke="white" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" style="filter: drop-shadow(0px 1px 2px rgba(0,0,0,0.8));">
          <g transform="rotate(${Math.round(angle)} 16 16)">
            <path d="M 23 9 A 10 10 0 0 1 23 23" />
            <path d="M 28 9 L 23 9 L 23 14" />
            <path d="M 28 23 L 23 23 L 23 18" />
          </g>
        </svg>`;
        const encodedSvg = encodeURIComponent(svgStr);
        stage.container().style.cursor = `url('data:image/svg+xml;utf8,${encodedSvg}') 16 16, crosshair`;
      },
      [cropX, cropY, scale],
    );

    const handleRotateStart = useCallback(
      (e: any) => {
        if (e.evt && typeof e.evt.button === 'number' && e.evt.button !== 0) return;

        isDragging.current = true;
        onMaskInteractionStart(e);
        e.cancelBubble = true;
        if (e.evt && e.evt.cancelable) e.evt.preventDefault();

        const stage = e.target.getStage();
        const pointer = getPointer(stage);
        if (!pointer) return;

        const cx = (pRef.current.centerX - cropX) * scale;
        const cy = (pRef.current.centerY - cropY) * scale;

        const startAngle = Math.atan2(pointer.y - cy, pointer.x - cx);
        rotateStartRef.current = {
          angle: startAngle,
          rotation: pRef.current.rotation || 0,
        };
      },
      [onMaskInteractionStart, cropX, cropY, scale, getPointer],
    );

    const handleRotateMove = useCallback(
      (e: any) => {
        if (!rotateStartRef.current) return;
        const stage = e.target.getStage();
        const pointer = getPointer(stage);
        if (!pointer) return;

        setRotateCursor(stage, pointer);

        const cx = (pRef.current.centerX - cropX) * scale;
        const cy = (pRef.current.centerY - cropY) * scale;

        const currentAngle = Math.atan2(pointer.y - cy, pointer.x - cx);
        const angleDiff = currentAngle - rotateStartRef.current.angle;
        const angleDiffDeg = (angleDiff * 180) / Math.PI;

        const newRotation = rotateStartRef.current.rotation + angleDiffDeg;

        const newP = {
          ...pRef.current,
          rotation: newRotation,
        };

        updateP(newP);
        if (onPreviewUpdate) onPreviewUpdate(subMask.id, { parameters: newP });
        onUpdate(subMask.id, { parameters: newP });
      },
      [cropX, cropY, scale, updateP, onPreviewUpdate, subMask.id, setRotateCursor, getPointer, onUpdate],
    );

    const handleRotateEnd = useCallback(
      (e: any) => {
        isDragging.current = false;
        rotateStartRef.current = null;
        onMaskInteractionEnd();
        onUpdate(subMask.id, { parameters: pRef.current });

        if (e?.target?.getStage) {
          e.target.getStage().container().style.cursor = '';
        }
      },
      [subMask.id, onMaskInteractionEnd, onUpdate],
    );

    const handleRotateHoverMove = useCallback(
      (e: any) => {
        if (isToolActive || isDragging.current) return;
        const stage = e.target.getStage();
        const pointer = getPointer(stage);
        if (pointer) setRotateCursor(stage, pointer);
      },
      [isToolActive, setRotateCursor, getPointer],
    );

    const handleRotateMouseEnter = useCallback(
      (e: any) => {
        onMaskMouseEnter();
        if (!isToolActive && !isDragging.current) {
          const stage = e.target.getStage();
          const pointer = getPointer(stage);
          if (pointer) setRotateCursor(stage, pointer);
        }
      },
      [onMaskMouseEnter, isToolActive, setRotateCursor, getPointer],
    );

    const handleRotateMouseLeave = useCallback(
      (e: any) => {
        onMaskMouseLeave();
        if (!isDragging.current) {
          const stage = e.target.getStage();
          stage.container().style.cursor = '';
        }
      },
      [onMaskMouseLeave],
    );

    const handleLinearGroupDragStart = useCallback(
      (e: any) => {
        if (e.evt && typeof e.evt.button === 'number' && e.evt.button !== 0) return;
        isDragging.current = true;
        onMaskInteractionStart(e);
        dragStartPointer.current = getPointer(e.target.getStage());
        dragStartParams.current = { ...pRef.current };
        e.cancelBubble = true;
      },
      [onMaskInteractionStart, getPointer],
    );

    const handleLinearGroupDragMove = useCallback(
      (e: any) => {
        const pointerPos = getPointer(e.target.getStage());
        if (!pointerPos || !dragStartPointer.current || !dragStartParams.current) return;

        const dx = (pointerPos.x - dragStartPointer.current.x) / scale;
        const dy = (pointerPos.y - dragStartPointer.current.y) / scale;

        const newP = {
          ...dragStartParams.current,
          startX: dragStartParams.current.startX + dx,
          startY: dragStartParams.current.startY + dy,
          endX: dragStartParams.current.endX + dx,
          endY: dragStartParams.current.endY + dy,
        };

        updateP(newP);
        if (onPreviewUpdate) onPreviewUpdate(subMask.id, { parameters: newP });
        onUpdate(subMask.id, { parameters: newP });
      },
      [scale, updateP, onPreviewUpdate, subMask.id, getPointer, onUpdate],
    );

    const handleLinearGroupDragEnd = useCallback(
      (e: any) => {
        isDragging.current = false;
        e.cancelBubble = true;
        onMaskInteractionEnd();
        onUpdate(subMask.id, { parameters: pRef.current });
      },
      [subMask.id, onMaskInteractionEnd, onUpdate],
    );

    const handleLinearPointDragStart = useCallback(
      (e: any) => {
        if (e.evt && typeof e.evt.button === 'number' && e.evt.button !== 0) return;
        isDragging.current = true;
        onMaskInteractionStart(e);
        e.cancelBubble = true;
      },
      [onMaskInteractionStart],
    );

    const handleLinearPointDragMove = useCallback(
      (e: any, pointType: string) => {
        const stage = e.target.getStage();
        const pointerPos = getPointer(stage);
        if (!pointerPos) return;

        const newX = pointerPos.x / scale + cropX;
        const newY = pointerPos.y / scale + cropY;

        const newP = { ...pRef.current };
        if (pointType === 'start') {
          newP.startX = newX;
          newP.startY = newY;
        } else {
          newP.endX = newX;
          newP.endY = newY;
        }
        updateP(newP);
        if (onPreviewUpdate) onPreviewUpdate(subMask.id, { parameters: newP });
        onUpdate(subMask.id, { parameters: newP });
      },
      [scale, cropX, cropY, updateP, onPreviewUpdate, subMask.id, getPointer, onUpdate],
    );

    const handleLinearRangeDragMove = useCallback(
      (e: any) => {
        const stage = e.target.getStage();
        const pointerPos = getPointer(stage);
        if (!pointerPos) return;

        const { startX, startY, endX, endY } = pRef.current;
        const sX = (startX - cropX) * scale;
        const sY = (startY - cropY) * scale;
        const eX = (endX - cropX) * scale;
        const eY = (endY - cropY) * scale;

        const dx = eX - sX;
        const dy = eY - sY;
        const len = Math.sqrt(dx * dx + dy * dy);

        let newRange = pRef.current.range;
        if (len > 0) {
          const dist = Math.abs(dx * (sY - pointerPos.y) - (sX - pointerPos.x) * dy) / len;
          newRange = Math.max(0.1, dist / scale);
        }

        const newP = { ...pRef.current, range: newRange };
        updateP(newP);
        if (onPreviewUpdate) onPreviewUpdate(subMask.id, { parameters: newP });

        onUpdate(subMask.id, { parameters: newP });
      },
      [scale, cropX, cropY, updateP, onPreviewUpdate, subMask.id, getPointer, onUpdate],
    );

    const handleLinearPointDragEnd = useCallback(
      (e: any) => {
        isDragging.current = false;
        e.cancelBubble = true;
        onMaskInteractionEnd();
        onUpdate(subMask.id, { parameters: pRef.current });
      },
      [subMask.id, onMaskInteractionEnd, onUpdate],
    );

    // ============= BLITZRAW: dragging a pen anchor or one of its handles =============
    // Konva moves the dragged node itself, and the path is drawn from the data
    // rather than from the node, so every move reads the node's new position,
    // writes it back into the parameters and leaves the node where the data
    // now says it is. `lockDragBoundFunc` pins the node so the two can never
    // disagree by a frame.
    const penPointerToPhoto = useCallback(
      (e: any) => {
        const stage = e.target.getStage();
        if (!stage) return null;
        const pos = getPointer(stage);
        if (!pos) return null;
        return { x: pos.x / scale + cropX, y: pos.y / scale + cropY };
      },
      [getPointer, scale, cropX, cropY],
    );

    const handlePenDragStart = useCallback(
      (e: any) => {
        if (e.evt && typeof e.evt.button === 'number' && e.evt.button !== 0) return;
        isDragging.current = true;
        e.cancelBubble = true;
        onMaskInteractionStart(e);
      },
      [onMaskInteractionStart],
    );

    const handlePenAnchorDragMove = useCallback(
      (e: any, index: number) => {
        e.cancelBubble = true;
        const photo = penPointerToPhoto(e);
        if (!photo) return;

        const points: Array<PenAnchor> = [...(pRef.current.points || [])];
        if (!points[index]) return;
        points[index] = moveAnchor(points[index], photo.x, photo.y);

        const next = { ...pRef.current, points };
        updateP(next);
        onPreviewUpdate?.(subMask.id, { parameters: next });
      },
      [penPointerToPhoto, updateP, onPreviewUpdate, subMask.id],
    );

    const handlePenHandleDragMove = useCallback(
      (e: any, index: number, which: 'handleIn' | 'handleOut') => {
        e.cancelBubble = true;
        const photo = penPointerToPhoto(e);
        if (!photo) return;

        const points: Array<PenAnchor> = [...(pRef.current.points || [])];
        const anchor = points[index];
        if (!anchor) return;

        // The far handle follows, mirrored, so a curve stays smooth through
        // its anchor. Alt breaks the pair and moves only the one being
        // dragged, which is how a curve is turned into a corner and the only
        // way to get a cusp at all.
        const breakPair = e.evt?.altKey === true;
        const other = which === 'handleIn' ? 'handleOut' : 'handleIn';

        const updated: PenAnchor = { ...anchor, [which]: { x: photo.x, y: photo.y } };
        if (!breakPair && anchor[other]) {
          updated[other] = mirrorHandle(anchor, photo);
        }
        points[index] = updated;

        const next = { ...pRef.current, points };
        updateP(next);
        onPreviewUpdate?.(subMask.id, { parameters: next });
      },
      [penPointerToPhoto, updateP, onPreviewUpdate, subMask.id],
    );

    const handlePenDragEnd = useCallback(
      (e: any) => {
        isDragging.current = false;
        e.cancelBubble = true;
        onMaskInteractionEnd();
        onUpdate(subMask.id, { parameters: pRef.current });
      },
      [subMask.id, onMaskInteractionEnd, onUpdate],
    );

    /**
     * Ctrl takes a point out, Alt turns it between a corner and a curve.
     *
     * Both live in the press rather than the click, because the point is
     * draggable and a press would otherwise start a drag that a click then has
     * to undo. Cancelling the bubble stops the press reaching the stage under
     * it as well.
     */
    const handlePenAnchorMouseDown = useCallback(
      (e: any, index: number) => {
        const ctrl = e.evt?.ctrlKey || e.evt?.metaKey;
        const alt = e.evt?.altKey;
        if (!ctrl && !alt) {
          return;
        }

        e.cancelBubble = true;
        if (e.evt?.preventDefault) e.evt.preventDefault();

        const points: Array<PenAnchor> = pRef.current.points || [];
        const updated = ctrl ? removeAnchor(points, index) : toggleAnchorSmooth(points, index);
        if (updated === points) {
          return;
        }

        const next = { ...pRef.current, points: updated };
        updateP(next);
        onUpdate(subMask.id, { parameters: next });
      },
      [updateP, onUpdate, subMask.id],
    );

    /**
     * Ctrl on the outline puts a point where the cursor is.
     *
     * The split is exact, so the line does not move when the point appears;
     * see `splitSegment`. Which segment was clicked is worked out from the
     * pointer rather than from the shape, because a single Shape draws the
     * whole path and Konva can only say that something on it was hit.
     */
    const handlePenPathMouseDown = useCallback(
      (e: any) => {
        const ctrl = e.evt?.ctrlKey || e.evt?.metaKey;
        if (!ctrl) {
          return;
        }

        const photo = penPointerToPhoto(e);
        if (!photo) return;

        const points: Array<PenAnchor> = pRef.current.points || [];
        const hit = closestSegment(points, true, photo.x, photo.y);
        if (!hit) return;

        e.cancelBubble = true;
        if (e.evt?.preventDefault) e.evt.preventDefault();

        const next = { ...pRef.current, points: splitSegment(points, hit.segmentIndex, hit.t) };
        updateP(next);
        onUpdate(subMask.id, { parameters: next });
      },
      [penPointerToPhoto, updateP, onUpdate, subMask.id],
    );

    /**
     * The two gestures that act on the whole path rather than one point of it.
     *
     * Ctrl and drag moves it. Alt and click copies it: the copy lands on top of
     * the original and becomes the selected one, so the Ctrl drag that usually
     * follows carries the copy and leaves the original where it was. Two
     * gestures rather than one Alt-drag, because the copy has to exist and be
     * selected before anything can drag it, and doing both inside one press
     * means the drag would still belong to the shape that was pressed.
     */
    const penBodyDrag = useRef<{ start: Coord; points: Array<PenAnchor> } | null>(null);

    const handlePenBodyMouseDown = useCallback(
      (e: any) => {
        const ctrl = e.evt?.ctrlKey || e.evt?.metaKey;
        const alt = e.evt?.altKey;
        if (!ctrl && !alt) {
          return;
        }

        const photo = penPointerToPhoto(e);
        if (!photo) return;

        e.cancelBubble = true;
        if (e.evt?.preventDefault) e.evt.preventDefault();

        if (alt && onDuplicateSubMask) {
          onDuplicateSubMask(subMask);
          return;
        }

        isDragging.current = true;
        onMaskInteractionStart(e);
        penBodyDrag.current = { start: photo, points: pRef.current.points || [] };
      },
      [penPointerToPhoto, onMaskInteractionStart, onDuplicateSubMask, subMask],
    );

    const handlePenBodyMouseMove = useCallback(
      (e: any) => {
        if (!penBodyDrag.current) return;
        const photo = penPointerToPhoto(e);
        if (!photo) return;

        e.cancelBubble = true;
        const { start, points } = penBodyDrag.current;
        const next = { ...pRef.current, points: translatePath(points, photo.x - start.x, photo.y - start.y) };
        updateP(next);
        onPreviewUpdate?.(subMask.id, { parameters: next });
      },
      [penPointerToPhoto, updateP, onPreviewUpdate, subMask.id],
    );

    const handlePenBodyMouseUp = useCallback(() => {
      if (!penBodyDrag.current) return;
      penBodyDrag.current = null;
      isDragging.current = false;
      onMaskInteractionEnd();
      onUpdate(subMask.id, { parameters: pRef.current });
    }, [onMaskInteractionEnd, onUpdate, subMask.id]);
    // =========== BLITZRAW END: dragging a pen anchor or one of its handles ===========

    if (!subMask.visible) {
      return null;
    }

    const commonProps = {
      dash: [4, 4],
      onClick: handleSelect,
      onTap: handleSelect,
      opacity: isSelected ? 1 : 0.7,
      stroke: isSelected
        ? '#0ea5e9'
        : subMask.mode === SubMaskMode.Subtractive
          ? '#f43f5e'
          : subMask.mode === SubMaskMode.Intersect
            ? '#a855f7'
            : 'white',
      strokeScaleEnabled: false,
      strokeWidth: isSelected ? 3 : 2,
    };

    if (subMask.type === Mask.AiSubject || subMask.type === Mask.QuickEraser) {
      const { startX, startY, endX, endY } = p;
      if (startX !== undefined && startY !== undefined && endX !== undefined && endY !== undefined) {
        const isPoint = Math.abs(startX - endX) < 1e-6 && Math.abs(startY - endY) < 1e-6;
        if (isPoint) {
          return (
            <Circle
              x={(startX - cropX) * scale}
              y={(startY - cropY) * scale}
              radius={5}
              stroke={isSelected ? '#0ea5e9' : 'white'}
              strokeWidth={2}
              listening={!isToolActive}
              onClick={handleSelect}
              onTap={handleSelect}
              onTouchEnd={handleMaskTouchEnd}
              onTouchStart={handleMaskTouchStart}
              onMouseEnter={onMaskMouseEnter}
              onMouseLeave={onMaskMouseLeave}
              shadowColor="black"
              shadowBlur={2}
              shadowOpacity={0.8}
            />
          );
        } else {
          return (
            <Rect
              height={Math.max(0.1, Math.abs(endY - startY) * scale)}
              onMouseEnter={onMaskMouseEnter}
              onMouseLeave={onMaskMouseLeave}
              onTouchEnd={handleMaskTouchEnd}
              onTouchStart={handleMaskTouchStart}
              width={Math.max(0.1, Math.abs(endX - startX) * scale)}
              x={(Math.min(startX, endX) - cropX) * scale}
              y={(Math.min(startY, endY) - cropY) * scale}
              {...commonProps}
            />
          );
        }
      }
      return null;
    }

    if (
      subMask.type === Mask.Brush ||
      subMask.type === Mask.Flow ||
      subMask.type === Mask.Clone ||
      subMask.type === Mask.Heal
    ) {
      const { lines = [], sourceX, sourceY } = p;

      let dx = 0;
      let dy = 0;
      let hasSource = false;

      if (
        (subMask.type === Mask.Clone || subMask.type === Mask.Heal) &&
        sourceX !== undefined &&
        sourceY !== undefined &&
        lines.length > 0
      ) {
        let minX = Infinity,
          minY = Infinity,
          maxX = -Infinity,
          maxY = -Infinity;
        for (const line of lines) {
          for (const pt of line.points) {
            if (pt.x < minX) minX = pt.x;
            if (pt.x > maxX) maxX = pt.x;
            if (pt.y < minY) minY = pt.y;
            if (pt.y > maxY) maxY = pt.y;
          }
        }
        if (minX !== Infinity) {
          const cx = (minX + maxX) / 2;
          const cy = (minY + maxY) / 2;
          dx = sourceX - cx;
          dy = sourceY - cy;
          hasSource = true;
        }
      }

      return (
        <Group
          onClick={handleSelect}
          onTap={handleSelect}
          onTouchEnd={handleMaskTouchEnd}
          onTouchStart={handleMaskTouchStart}
        >
          <Group visible={showBrushStrokes !== false}>
            {lines.map((line: DrawnLine, i: number) => (
              <OptimizedBrushLine key={i} line={line} scale={scale} cropX={cropX} cropY={cropY} />
            ))}

            {hasSource &&
              isSelected &&
              lines.map((line: DrawnLine, i: number) => (
                <SourcePreviewLine
                  key={`source-${i}`}
                  line={line}
                  scale={scale}
                  cropX={cropX}
                  cropY={cropY}
                  dx={dx}
                  dy={dy}
                />
              ))}
          </Group>

          {sourceX !== undefined && sourceY !== undefined && isSelected && (
            <Group x={(sourceX - cropX) * scale} y={(sourceY - cropY) * scale}>
              <Circle
                radius={6 / stageScale}
                stroke="white"
                strokeWidth={2 / stageScale}
                shadowColor="black"
                shadowBlur={2 / stageScale}
              />
              <Circle
                radius={6 / stageScale}
                stroke="black"
                strokeWidth={1 / stageScale}
                dash={[2 / stageScale, 2 / stageScale]}
              />
              <Line
                points={[-10 / stageScale, 0, 10 / stageScale, 0]}
                stroke="white"
                strokeWidth={1.5 / stageScale}
                shadowColor="black"
                shadowBlur={2 / stageScale}
              />
              <Line
                points={[0, -10 / stageScale, 0, 10 / stageScale]}
                stroke="white"
                strokeWidth={1.5 / stageScale}
                shadowColor="black"
                shadowBlur={2 / stageScale}
              />
            </Group>
          )}
        </Group>
      );
    }

    // ==================== BLITZRAW: the pen mask overlay ====================
    // The path, its anchors and its handles. It is drawn with a Shape and a
    // scene function rather than a Konva Line, because a Line cannot mix a
    // straight run and a curve in the same outline and the whole point of the
    // pen is that one path does both.
    //
    // Handles only appear on the anchors either side of the one being worked
    // on, plus the last placed. Showing every handle on a twenty point path
    // buries the picture under furniture, and the ones far from the cursor are
    // never the ones being reached for.
    if (subMask.type === Mask.Pen) {
      const points: Array<PenAnchor> = p.points || [];
      const isPlacing = p.isDrawing === true;

      if (points.length === 0) {
        return null;
      }

      const toCanvas = (pt: { x: number; y: number }) => ({
        x: (pt.x - cropX) * scale,
        y: (pt.y - cropY) * scale,
      });

      // Everything on top of the photo is sized against the zoom, so a handle
      // stays the same size on screen however far in the picture is scaled.
      const px = (n: number) => n / stageScale;

      const strokeColour = isSelected ? '#0ea5e9' : 'white';
      const showFurniture = isSelected || isPlacing;

      return (
        <Group>
          {/* The inside of the path, invisible but hit-testable. It carries the
              two whole-path gestures, Ctrl to move and Alt to copy, and it is
              also what makes clicking anywhere in a shape select it rather than
              having to find its outline. Only once the path is finished: while
              placing, every click belongs to the next point. */}
          {!isPlacing && (
            <Shape
              sceneFunc={(ctx: any, shape: any) => {
                tracePenPath(ctx, points, toCanvas, true);
                ctx.fillShape(shape);
              }}
              fill="rgba(0,0,0,0.001)"
              listening={!isToolActive}
              onClick={handleSelect}
              onTap={handleSelect}
              onMouseDown={handlePenBodyMouseDown}
              onMouseMove={handlePenBodyMouseMove}
              onMouseUp={handlePenBodyMouseUp}
              onTouchEnd={handleMaskTouchEnd}
              onTouchStart={handleMaskTouchStart}
              onMouseEnter={onMaskMouseEnter}
              onMouseLeave={(e: any) => {
                onMaskMouseLeave();
                handlePenBodyMouseUp();
                e.target.getStage().container().style.cursor = '';
              }}
            />
          )}

          <Shape
            sceneFunc={(ctx: any, shape: any) => {
              tracePenPath(ctx, points, toCanvas, p.closed === true || !isPlacing);
              ctx.strokeShape(shape);
            }}
            stroke={strokeColour}
            strokeWidth={px(isSelected ? 2 : 1.5)}
            shadowColor="black"
            shadowBlur={px(2)}
            shadowOpacity={0.8}
            hitStrokeWidth={px(16)}
            listening={!isToolActive}
            onClick={handleSelect}
            onTap={handleSelect}
            onMouseDown={handlePenPathMouseDown}
            onTouchEnd={handleMaskTouchEnd}
            onTouchStart={handleMaskTouchStart}
            onMouseEnter={(e: any) => {
              onMaskMouseEnter();
              if (!isPlacing) e.target.getStage().container().style.cursor = 'copy';
            }}
            onMouseLeave={(e: any) => {
              onMaskMouseLeave();
              e.target.getStage().container().style.cursor = '';
            }}
          />

          {/* The rubber band from the last placed anchor to the cursor. It is
              the one piece of the path that is not in the data yet, so it is
              drawn separately and only while placing. */}
          {isPlacing && penCursor && points.length > 0 && (
            <Line
              points={[
                toCanvas(points[points.length - 1]).x,
                toCanvas(points[points.length - 1]).y,
                penCursor.x,
                penCursor.y,
              ]}
              stroke={strokeColour}
              strokeWidth={px(1)}
              dash={[px(4), px(4)]}
              opacity={0.7}
              listening={false}
            />
          )}

          {showFurniture &&
            points.map((anchor: PenAnchor, index: number) => {
              const a = toCanvas(anchor);
              const isFirst = index === 0;
              const canClose = isPlacing && isFirst && points.length >= MIN_ANCHORS_FOR_AREA;
              const showHandles =
                !isPlacing || index === points.length - 1 || index === penActiveAnchor;

              return (
                <Group key={`pen-${index}`}>
                  {showHandles && anchor.handleIn && (
                    <>
                      <Line
                        points={[a.x, a.y, toCanvas(anchor.handleIn).x, toCanvas(anchor.handleIn).y]}
                        stroke="white"
                        strokeWidth={px(1)}
                        opacity={0.7}
                        listening={false}
                      />
                      <Circle
                        x={toCanvas(anchor.handleIn).x}
                        y={toCanvas(anchor.handleIn).y}
                        radius={px(4)}
                        fill="white"
                        stroke="black"
                        strokeWidth={px(1)}
                        draggable={!isPlacing}
                        dragBoundFunc={lockDragBoundFunc}
                        onDragStart={handlePenDragStart}
                        onDragMove={(e: any) => handlePenHandleDragMove(e, index, 'handleIn')}
                        onDragEnd={handlePenDragEnd}
                      />
                    </>
                  )}

                  {showHandles && anchor.handleOut && (
                    <>
                      <Line
                        points={[a.x, a.y, toCanvas(anchor.handleOut).x, toCanvas(anchor.handleOut).y]}
                        stroke="white"
                        strokeWidth={px(1)}
                        opacity={0.7}
                        listening={false}
                      />
                      <Circle
                        x={toCanvas(anchor.handleOut).x}
                        y={toCanvas(anchor.handleOut).y}
                        radius={px(4)}
                        fill="white"
                        stroke="black"
                        strokeWidth={px(1)}
                        draggable={!isPlacing}
                        dragBoundFunc={lockDragBoundFunc}
                        onDragStart={handlePenDragStart}
                        onDragMove={(e: any) => handlePenHandleDragMove(e, index, 'handleOut')}
                        onDragEnd={handlePenDragEnd}
                      />
                    </>
                  )}

                  {/* Square for an anchor, round for a handle, the way every
                      vector editor tells the two apart. The first anchor grows
                      while placing, because it is the target that ends the
                      path and it has to be findable without aiming. */}
                  <Rect
                    x={a.x - px(canClose ? 5 : 3.5)}
                    y={a.y - px(canClose ? 5 : 3.5)}
                    width={px(canClose ? 10 : 7)}
                    height={px(canClose ? 10 : 7)}
                    fill={isFirst && isPlacing ? '#0ea5e9' : 'white'}
                    stroke="black"
                    strokeWidth={px(1)}
                    draggable={!isPlacing}
                    dragBoundFunc={lockDragBoundFunc}
                    onMouseDown={(e: any) => handlePenAnchorMouseDown(e, index)}
                    onDragStart={handlePenDragStart}
                    onDragMove={(e: any) => handlePenAnchorDragMove(e, index)}
                    onDragEnd={handlePenDragEnd}
                    onMouseEnter={(e: any) => {
                      onMaskMouseEnter();
                      if (!isPlacing) e.target.getStage().container().style.cursor = 'move';
                    }}
                    onMouseLeave={(e: any) => {
                      onMaskMouseLeave();
                      e.target.getStage().container().style.cursor = '';
                    }}
                  />
                </Group>
              );
            })}
        </Group>
      );
    }
    // ================== BLITZRAW END: the pen mask overlay ==================

    if (subMask.type === Mask.Radial) {
      const { centerX, centerY, radiusX, radiusY, rotation } = p;
      if (p.isInitialDraw && (radiusX < 1 || radiusY < 2)) return null;

      return (
        <Group>
          {isSelected && !isToolActive && (
            <Ellipse
              x={(centerX - cropX) * scale}
              y={(centerY - cropY) * scale}
              radiusX={Math.max(0.1, radiusX * scale) + 35}
              radiusY={Math.max(0.1, radiusY * scale) + 35}
              rotation={rotation}
              fill="transparent"
              draggable
              dragBoundFunc={lockDragBoundFunc}
              onDragStart={handleRotateStart}
              onDragMove={handleRotateMove}
              onDragEnd={handleRotateEnd}
              onMouseEnter={handleRotateMouseEnter}
              onMouseMove={handleRotateHoverMove}
              onMouseLeave={handleRotateMouseLeave}
              onTouchStart={handleRotateStart}
              onTouchMove={handleRotateMove}
              onTouchEnd={handleRotateEnd}
            />
          )}

          <Ellipse
            {...commonProps}
            ref={shapeRef}
            fill="transparent"
            draggable={!isToolActive}
            dragBoundFunc={lockDragBoundFunc}
            onDragStart={handleRadialDragStart}
            onDragMove={handleRadialDragMove}
            onDragEnd={handleRadialDragEnd}
            onMouseEnter={(e: any) => {
              onMaskMouseEnter();
              if (!isToolActive && !isDragging.current) {
                e.target.getStage().container().style.cursor = 'move';
              }
            }}
            onMouseLeave={(e: any) => {
              onMaskMouseLeave();
              if (!isDragging.current && e?.target?.getStage) {
                e.target.getStage().container().style.cursor = '';
              }
            }}
            onTouchEnd={handleMaskTouchEnd}
            onTouchStart={handleMaskTouchStart}
            radiusX={Math.max(0.1, radiusX * scale)}
            radiusY={Math.max(0.1, radiusY * scale)}
            rotation={rotation}
            x={(centerX - cropX) * scale}
            y={(centerY - cropY) * scale}
          />
          {isSelected && !isToolActive && (
            <Transformer
              ref={trRef}
              centeredScaling={true}
              rotateEnabled={false}
              enabledAnchors={[
                'top-left',
                'top-right',
                'bottom-left',
                'bottom-right',
                'top-center',
                'bottom-center',
                'middle-left',
                'middle-right',
              ]}
              onMouseDown={(e) => {
                if (e.evt && typeof e.evt.button === 'number' && e.evt.button !== 0) return;
                e.cancelBubble = true;
                e.evt.preventDefault();
              }}
              onTouchStart={(e) => {
                handleMaskTouchStart(e);
                e.cancelBubble = true;
                e.evt.preventDefault();
              }}
              onTouchEnd={handleMaskTouchEnd}
              boundBoxFunc={(oldBox, newBox) => {
                if (Math.abs(newBox.width) < 5 || Math.abs(newBox.height) < 5) {
                  return oldBox;
                }
                return newBox;
              }}
              onTransformStart={handleRadialTransformStart}
              onTransform={handleRadialTransform}
              onTransformEnd={handleRadialTransformEnd}
              onMouseEnter={onMaskMouseEnter}
              onMouseLeave={onMaskMouseLeave}
            />
          )}
        </Group>
      );
    }

    if (subMask.type === Mask.Linear) {
      const defaultRange = Math.min(cropW, cropH) * 0.1;
      const { startX, startY, endX, endY, range = defaultRange } = p;

      const flickDistX = startX - endX;
      const flickDistY = startY - endY;
      if (p.isInitialDraw && Math.sqrt(flickDistX * flickDistX + flickDistY * flickDistY) < 1) return null;

      const sX = (startX - cropX) * scale;
      const sY = (startY - cropY) * scale;
      const eX = (endX - cropX) * scale;
      const eY = (endY - cropY) * scale;
      const r = range * scale;

      const idx = endX - startX;
      const idy = endY - startY;
      const angle = Math.atan2(idy, idx);
      const angleDeg = (angle * 180) / Math.PI;

      const centerX = sX + (eX - sX) / 2;
      const centerY = sY + (eY - sY) / 2;

      const nx = -Math.sin(angle);
      const ny = Math.cos(angle);
      const dx_norm = Math.cos(angle);
      const dy_norm = Math.sin(angle);

      const EXT = 5000;
      const topRangePts = [
        sX + nx * r - dx_norm * EXT,
        sY + ny * r - dy_norm * EXT,
        eX + nx * r + dx_norm * EXT,
        eY + ny * r + dy_norm * EXT,
      ];
      const botRangePts = [
        sX - nx * r - dx_norm * EXT,
        sY - ny * r - dy_norm * EXT,
        eX - nx * r + dx_norm * EXT,
        eY - ny * r + dy_norm * EXT,
      ];

      const lineProps = {
        ...commonProps,
        strokeWidth: isSelected ? 2.5 : 2,
        dash: [6, 6],
        hitStrokeWidth: 40,
      };

      const showFeatherLines = isSelected && (!isToolActive || p.isInitialDraw);

      return (
        <Group>
          <Group
            x={centerX}
            y={centerY}
            rotation={angleDeg}
            draggable={isSelected && !isToolActive}
            dragBoundFunc={lockDragBoundFunc}
            onDragStart={handleLinearGroupDragStart}
            onDragMove={handleLinearGroupDragMove}
            onDragEnd={handleLinearGroupDragEnd}
            onClick={handleSelect}
            onTap={handleSelect}
            onTouchEnd={handleMaskTouchEnd}
            onTouchStart={handleMaskTouchStart}
            onMouseEnter={(e: any) => {
              onMaskMouseEnter();
              if (!isToolActive) e.target.getStage().container().style.cursor = 'move';
            }}
            onMouseLeave={(e: any) => {
              onMaskMouseLeave();
              e.target.getStage().container().style.cursor = '';
            }}
          >
            <Line points={[-5000, 0, 5000, 0]} {...lineProps} dash={[2, 3]} />
          </Group>

          {showFeatherLines && (
            <>
              <Line
                points={topRangePts}
                {...lineProps}
                draggable={!isToolActive}
                dragBoundFunc={lockDragBoundFunc}
                onDragStart={handleLinearPointDragStart}
                onDragMove={handleLinearRangeDragMove}
                onDragEnd={handleLinearPointDragEnd}
                onTouchEnd={handleMaskTouchEnd}
                onTouchStart={handleMaskTouchStart}
                onMouseEnter={(e: any) => {
                  onMaskMouseEnter();
                  if (!isToolActive) e.target.getStage().container().style.cursor = 'row-resize';
                }}
                onMouseLeave={(e: any) => {
                  onMaskMouseLeave();
                  e.target.getStage().container().style.cursor = '';
                }}
              />
              <Line
                points={botRangePts}
                {...lineProps}
                draggable={!isToolActive}
                dragBoundFunc={lockDragBoundFunc}
                onDragStart={handleLinearPointDragStart}
                onDragMove={handleLinearRangeDragMove}
                onDragEnd={handleLinearPointDragEnd}
                onTouchEnd={handleMaskTouchEnd}
                onTouchStart={handleMaskTouchStart}
                onMouseEnter={(e: any) => {
                  onMaskMouseEnter();
                  if (!isToolActive) e.target.getStage().container().style.cursor = 'row-resize';
                }}
                onMouseLeave={(e: any) => {
                  onMaskMouseLeave();
                  e.target.getStage().container().style.cursor = '';
                }}
              />
            </>
          )}

          {isSelected && !isToolActive && (
            <>
              <Circle
                x={sX}
                y={sY}
                radius={8 / stageScale}
                fill="#0ea5e9"
                stroke="white"
                strokeWidth={2 / stageScale}
                draggable
                dragBoundFunc={lockDragBoundFunc}
                onDragStart={handleLinearPointDragStart}
                onDragMove={(e) => handleLinearPointDragMove(e, 'start')}
                onDragEnd={handleLinearPointDragEnd}
                onTouchEnd={handleMaskTouchEnd}
                onTouchStart={handleMaskTouchStart}
                onMouseEnter={(e: any) => {
                  onMaskMouseEnter();
                  e.target.getStage().container().style.cursor = 'grab';
                }}
                onMouseLeave={(e: any) => {
                  onMaskMouseLeave();
                  e.target.getStage().container().style.cursor = '';
                }}
              />
              <Circle
                x={eX}
                y={eY}
                radius={8 / stageScale}
                fill="#0ea5e9"
                stroke="white"
                strokeWidth={2 / stageScale}
                draggable
                dragBoundFunc={lockDragBoundFunc}
                onDragStart={handleLinearPointDragStart}
                onDragMove={(e) => handleLinearPointDragMove(e, 'end')}
                onDragEnd={handleLinearPointDragEnd}
                onTouchEnd={handleMaskTouchEnd}
                onTouchStart={handleMaskTouchStart}
                onMouseEnter={(e: any) => {
                  onMaskMouseEnter();
                  e.target.getStage().container().style.cursor = 'grab';
                }}
                onMouseLeave={(e: any) => {
                  onMaskMouseLeave();
                  e.target.getStage().container().style.cursor = '';
                }}
              />
            </>
          )}

          {!isSelected && (
            <>
              <Line
                points={topRangePts}
                {...lineProps}
                opacity={0.7}
                stroke="white"
                listening={true}
                onClick={handleSelect}
                onTap={handleSelect}
                onTouchEnd={handleMaskTouchEnd}
                onTouchStart={handleMaskTouchStart}
                onMouseEnter={(e: any) => {
                  onMaskMouseEnter();
                  if (!isToolActive) e.target.getStage().container().style.cursor = 'row-resize';
                }}
                onMouseLeave={(e: any) => {
                  onMaskMouseLeave();
                  e.target.getStage().container().style.cursor = '';
                }}
              />
              <Line
                points={botRangePts}
                {...lineProps}
                opacity={0.7}
                stroke="white"
                listening={true}
                onClick={handleSelect}
                onTap={handleSelect}
                onTouchEnd={handleMaskTouchEnd}
                onTouchStart={handleMaskTouchStart}
                onMouseEnter={(e: any) => {
                  onMaskMouseEnter();
                  if (!isToolActive) e.target.getStage().container().style.cursor = 'row-resize';
                }}
                onMouseLeave={(e: any) => {
                  onMaskMouseLeave();
                  e.target.getStage().container().style.cursor = '';
                }}
              />
            </>
          )}
        </Group>
      );
    }

    if (subMask.type === Mask.Color || subMask.type === Mask.Luminance) {
      const { targetX, targetY } = p;
      if (targetX !== undefined && targetX >= 0 && targetY !== undefined && targetY >= 0) {
        return (
          <Circle
            x={(targetX - cropX) * scale}
            y={(targetY - cropY) * scale}
            radius={5}
            stroke={isSelected ? '#0ea5e9' : 'white'}
            strokeWidth={2}
            listening={false}
            onTouchEnd={handleMaskTouchEnd}
            onTouchStart={handleMaskTouchStart}
            shadowColor="black"
            shadowBlur={2}
            shadowOpacity={0.8}
          />
        );
      }
      return null;
    }
    return null;
  },
);

const ImageCanvas = memo(
  ({
    appSettings,
    activeAiPatchContainerId,
    activeAiSubMaskId,
    activeMaskContainerId,
    activeMaskId,
    adjustments,
    brushSettings,
    crop,
    finalPreviewUrl,
    cachedPreviewUrl,
    handleCropComplete,
    imageRenderSize,
    interactivePatch,
    isAiEditing,
    isCropping,
    isMaskControlHovered,
    isMaskOverlayWanted,
    isMasking,
    isSliderDragging,
    isStraightenActive,
    isRotationActive,
    maskOverlayUrl,
    onGenerateAiMask,
    onLiveMaskPreview,
    onManualCleanup,
    onQuickErase,
    onSelectAiSubMask,
    onSelectMask,
    onSelectAiPatchContainer,
    onSelectMaskContainer,
    onStraighten,
    selectedImage,
    setCrop,
    setIsMaskHovered,
    setIsMaskTouchInteracting,
    showOriginal,
    transformedOriginalUrl,
    uncroppedAdjustedPreviewUrl,
    updateSubMask,
    isWbPickerActive = false,
    onWbPicked,
    setAdjustments,
    overlayRotation,
    overlayMode,
    cursorStyle,
    isMaxZoom,
    liveRotation,
    transformState,
    hasRenderedFirstFrame,
  }: ImageCanvasProps) => {
    const [isCropViewVisible, setIsCropViewVisible] = useState(false);
    const cropImageRef = useRef<HTMLImageElement>(null);
    const [displayedMaskUrl, setDisplayedMaskUrl] = useState<string | null>(null);
    const [originalLoaded, setOriginalLoaded] = useState<boolean>(false);
    const [localInitialDrawParams, setLocalInitialDrawParams] = useState<any>(null);
    const [hoveredMarkerId, setHoveredMarkerId] = useState<string | null>(null);
    const isDrawing = useRef(false);
    const drawingStageRef = useRef<any>(null);
    const dragStartPointer = useRef<Coord | null>(null);
    const lastBrushPoint = useRef<Coord | null>(null);
    const currentLine = useRef<DrawnLine | null>(null);
    const previewBoxRef = useRef<{ start: Coord; end: Coord } | null>(null);
    const [previewBox, setPreviewBox] = useState<{ start: Coord; end: Coord } | null>(null);
    const activeStrokeIndex = useRef<number | null>(null);

    const [cursorPreview, setCursorPreview] = useState<CursorPreview>({ x: 0, y: 0, visible: false });

    // ================== BLITZRAW: what the pen needs while placing ==================
    // The rubber band's far end, and which anchor is having its handles pulled.
    // Both are about the gesture rather than the path, so neither belongs in
    // the mask's parameters: nothing here should end up in a sidecar.
    const [penCursor, setPenCursor] = useState<Coord | null>(null);
    const penDrag = useRef<{ index: number; start: Coord } | null>(null);
    // ================ BLITZRAW END: what the pen needs while placing ================

    const [straightenLine, setStraightenLine] = useState<any>(null);
    const isStraightening = useRef(false);

    const [displayState, setDisplayState] = useState({
      base: finalPreviewUrl || cachedPreviewUrl || selectedImage.thumbnailUrl,
      fade: null as string | null,
    });
    const [isFadingIn, setIsFadingIn] = useState(false);
    const prevImageIdentityRef = useRef(selectedImage.thumbnailUrl);

    const [baseTool, setBaseTool] = useState<ToolType>(brushSettings?.tool ?? ToolType.Brush);
    const [isAltPressed, setIsAltPressed] = useState(false);
    const [isCtrlPressed, setIsCtrlPressed] = useState(false);
    const retainedPatchRef = useRef<typeof interactivePatch>(null);

    const isWgpuActive = appSettings?.useWgpuRenderer !== false && selectedImage?.isReady && hasRenderedFirstFrame;
    const { t } = useTranslation();
    const osPlatform = useOsPlatform();
    const modifierKey = osPlatform === 'macos' ? 'Cmd' : 'Ctrl';

    const manualCleanupStateRef = useRef({
      inFlight: false,
      pending: false,
      activeId: null as string | null,
      sourceX: 0,
      sourceY: 0,
    });

    const triggerManualCleanup = useCallback(
      async (activeId: string, sourceX: number, sourceY: number) => {
        if (!onManualCleanup) return;

        if (manualCleanupStateRef.current.inFlight) {
          manualCleanupStateRef.current.pending = true;
          manualCleanupStateRef.current.activeId = activeId;
          manualCleanupStateRef.current.sourceX = sourceX;
          manualCleanupStateRef.current.sourceY = sourceY;
          return;
        }

        manualCleanupStateRef.current.inFlight = true;
        manualCleanupStateRef.current.pending = false;

        try {
          await onManualCleanup(activeId, sourceX, sourceY);
        } finally {
          manualCleanupStateRef.current.inFlight = false;
          if (manualCleanupStateRef.current.pending && manualCleanupStateRef.current.activeId) {
            triggerManualCleanup(
              manualCleanupStateRef.current.activeId,
              manualCleanupStateRef.current.sourceX,
              manualCleanupStateRef.current.sourceY,
            );
          }
        }
      },
      [onManualCleanup],
    );

    const paddingX = imageRenderSize.width * 0.5;
    const paddingY = imageRenderSize.height * 0.5;

    const stageLeft = imageRenderSize.offsetX - paddingX;
    const stageTop = imageRenderSize.offsetY - paddingY;
    const stageWidth = imageRenderSize.width > 0 ? imageRenderSize.width + paddingX * 2 : 0;
    const stageHeight = imageRenderSize.height > 0 ? imageRenderSize.height + paddingY * 2 : 0;

    const groupOffsetX = paddingX;
    const groupOffsetY = paddingY;

    const [settledScale, setSettledScale] = useState(transformState.scale);
    useEffect(() => {
      const timer = setTimeout(() => {
        setSettledScale(transformState.scale);
      }, 150);
      return () => clearTimeout(timer);
    }, [transformState.scale]);

    const maxDimension = Math.max(stageWidth, stageHeight, 1);
    const maxSafeScale = Math.max(1, Math.min(settledScale, 4092 / maxDimension));

    const getCanvasPointer = useCallback(
      (stage: any) => {
        const pos = stage.getPointerPosition();
        if (!pos) return null;
        return {
          x: pos.x / maxSafeScale - groupOffsetX,
          y: pos.y / maxSafeScale - groupOffsetY,
        };
      },
      [groupOffsetX, groupOffsetY, maxSafeScale],
    );

    useEffect(() => {
      if (interactivePatch) {
        retainedPatchRef.current = interactivePatch;
      }
    }, [interactivePatch]);

    useEffect(() => {
      const newSrc = finalPreviewUrl || cachedPreviewUrl || selectedImage.thumbnailUrl;
      const isNewImage = prevImageIdentityRef.current !== selectedImage.thumbnailUrl;

      if (isNewImage) {
        prevImageIdentityRef.current = selectedImage.thumbnailUrl;
        setDisplayState({ base: newSrc, fade: null });
        setIsFadingIn(false);
        return;
      }

      // BLITZRAW: a clean cut. There was a 150ms crossfade here, which was a
      // reasonable way to cover a picture arriving late and became a smear the
      // moment previews on disk made it arrive at once. The fade layer is left
      // in place below and simply never asked for, so putting it back is one
      // branch rather than a rewrite.
      setDisplayState({ base: newSrc, fade: null });
      setIsFadingIn(false);
    }, [finalPreviewUrl, cachedPreviewUrl, selectedImage.thumbnailUrl, isSliderDragging]);

    useEffect(() => {
      setBaseTool(brushSettings?.tool ?? ToolType.Brush);
    }, [brushSettings?.tool]);

    useEffect(() => {
      const handleKeyDown = (e: KeyboardEvent) => {
        if (e.key === 'Alt') {
          e.preventDefault();
          (window as any).altKeyDown = true;
          setIsAltPressed(true);
        }
        if (e.key === 'Control' || e.key === 'Meta') {
          (window as any).ctrlKeyDown = true;
          setIsCtrlPressed(true);
        }
      };
      const handleKeyUp = (e: KeyboardEvent) => {
        if (e.key === 'Alt') {
          e.preventDefault();
          (window as any).altKeyDown = false;
          setIsAltPressed(false);
        }
        if (e.key === 'Control' || e.key === 'Meta') {
          (window as any).ctrlKeyDown = false;
          setIsCtrlPressed(false);
        }
      };
      const handleBlur = () => {
        (window as any).altKeyDown = false;
        setIsAltPressed(false);
        (window as any).ctrlKeyDown = false;
        setIsCtrlPressed(false);
      };

      window.addEventListener('keydown', handleKeyDown);
      window.addEventListener('keyup', handleKeyUp);
      window.addEventListener('blur', handleBlur);

      return () => {
        window.removeEventListener('keydown', handleKeyDown);
        window.removeEventListener('keyup', handleKeyUp);
        window.removeEventListener('blur', handleBlur);
        (window as any).altKeyDown = false;
        (window as any).ctrlKeyDown = false;
      };
    }, []);

    const activeContainer = useMemo(() => {
      if (isMasking) {
        return adjustments.masks.find((c: MaskContainer) => c.id === activeMaskContainerId);
      }
      if (isAiEditing) {
        return adjustments.aiPatches.find((p: AiPatch) => p.id === activeAiPatchContainerId);
      }
      return null;
    }, [
      adjustments.masks,
      adjustments.aiPatches,
      activeMaskContainerId,
      activeAiPatchContainerId,
      isMasking,
      isAiEditing,
    ]);

    const activeSubMask = useMemo(() => {
      if (!activeContainer) {
        return null;
      }
      if (isMasking) {
        return activeContainer.subMasks.find((m: SubMask) => m.id === activeMaskId);
      }
      if (isAiEditing) {
        return activeContainer.subMasks.find((m: SubMask) => m.id === activeAiSubMaskId);
      }
      return null;
    }, [activeContainer, activeMaskId, activeAiSubMaskId, isMasking, isAiEditing]);

    const effectiveImageDimensions = useMemo(() => {
      const steps = adjustments.orientationSteps || 0;
      const w = selectedImage.width || 0;
      const h = selectedImage.height || 0;
      if (steps === 1 || steps === 3) {
        return { width: h, height: w };
      }
      return { width: w, height: h };
    }, [selectedImage.width, selectedImage.height, adjustments.orientationSteps]);

    const activeCrop = adjustments.crop;
    const isPercentCrop = activeCrop?.unit === '%';
    const cropX = activeCrop
      ? isPercentCrop
        ? (activeCrop.x / 100) * effectiveImageDimensions.width
        : activeCrop.x
      : 0;
    const cropY = activeCrop
      ? isPercentCrop
        ? (activeCrop.y / 100) * effectiveImageDimensions.height
        : activeCrop.y
      : 0;

    const effectiveZoomScale = transformState.scale > 0 ? transformState.scale : 1;
    const brushStageSize = (brushSettings?.size ?? 0) / effectiveZoomScale;
    const brushImageSpaceSize = brushStageSize / (imageRenderSize.scale || 1);

    const isBrushActive =
      (isMasking || isAiEditing) &&
      (activeSubMask?.type === Mask.Brush ||
        activeSubMask?.type === Mask.Flow ||
        activeSubMask?.type === Mask.Clone ||
        activeSubMask?.type === Mask.Heal);
    const isManualCleanupActive =
      isAiEditing && (activeSubMask?.type === Mask.Clone || activeSubMask?.type === Mask.Heal);

    const isCloneOrHealActive =
      (isMasking || isAiEditing) && (activeSubMask?.type === Mask.Clone || activeSubMask?.type === Mask.Heal);

    const activeLineFlow = activeSubMask?.type === Mask.Flow ? (activeSubMask?.parameters?.flow ?? 10) : undefined;

    const brushCursorPreview = useMemo(() => {
      const radius = Math.max(0.1, brushStageSize / 2);
      const feather = Math.max(0, Math.min(1, (brushSettings?.feather ?? 0) / 100));
      const subMaskOpacity = Math.max(0, Math.min(1, (activeSubMask?.opacity ?? 100) / 100));
      const containerOpacity =
        activeContainer && 'opacity' in activeContainer && typeof activeContainer.opacity === 'number'
          ? Math.max(0, Math.min(1, activeContainer.opacity / 100))
          : 1;
      const flowOpacity =
        activeSubMask?.type === Mask.Flow ? Math.max(0, Math.min(1, (activeSubMask.parameters?.flow ?? 10) / 100)) : 1;
      const alpha = Math.max(0, Math.min(0.5, 0.5 * subMaskOpacity * containerOpacity * flowOpacity));

      const isEraser = isAltPressed ? baseTool !== ToolType.Eraser : baseTool === ToolType.Eraser;

      const strokeColor = isEraser
        ? (a: number) => `rgba(244, 63, 94, ${a.toFixed(3)})`
        : (a: number) => `rgba(14, 165, 233, ${a.toFixed(3)})`;

      if (feather <= 0.001) {
        return {
          fill: strokeColor(alpha),
          radius,
        };
      }

      const innerStop = 1 - feather;
      const colorStops: Array<number | string> = [0, strokeColor(alpha)];

      if (innerStop > 0.001) {
        colorStops.push(innerStop, strokeColor(alpha));
      }

      for (const t of [0.25, 0.5, 0.75, 1]) {
        const smoothstep = t * t * (3 - 2 * t);
        const intensity = 1 - smoothstep;
        colorStops.push(Math.min(1, innerStop + feather * t), strokeColor(alpha * intensity));
      }

      return {
        colorStops,
        radius,
      };
    }, [
      activeContainer,
      activeSubMask?.opacity,
      activeSubMask?.parameters?.flow,
      activeSubMask?.type,
      brushSettings?.feather,
      brushStageSize,
      baseTool,
      isAltPressed,
    ]);

    const isAiSubjectActive =
      (isMasking || isAiEditing) &&
      (activeSubMask?.type === Mask.AiSubject || activeSubMask?.type === Mask.QuickEraser);
    const isParametricActive =
      (isMasking || isAiEditing) && (activeSubMask?.type === Mask.Color || activeSubMask?.type === Mask.Luminance);
    const isInitialDrawing = (isMasking || isAiEditing) && activeSubMask?.parameters?.isInitialDraw === true;

    // ==================== BLITZRAW: the pen, while it is being placed ====================
    // Only the placing half counts as a tool. Once the path is finished it goes
    // back to being an ordinary shape whose anchors are dragged, and the photo
    // under it goes back to panning; keeping the tool active would mean every
    // click on the picture added a point to a path that was already done.
    const isPenPlacing =
      (isMasking || isAiEditing) && activeSubMask?.type === Mask.Pen && activeSubMask?.parameters?.isDrawing === true;

    const isToolActive =
      isBrushActive || isAiSubjectActive || isInitialDrawing || isParametricActive || isPenPlacing;

    useEffect(() => {
      if (maskOverlayUrl && (isMasking || isAiEditing)) {
        setDisplayedMaskUrl(maskOverlayUrl);
      } else {
        setDisplayedMaskUrl(null);
      }
    }, [maskOverlayUrl, isMasking, isAiEditing]);

    useEffect(() => {
      if (isToolActive) {
        return;
      }
      isDrawing.current = false;
      drawingStageRef.current = null;
      dragStartPointer.current = null;
      currentLine.current = null;
      lastBrushPoint.current = null;
      setPreviewBox(null);
      previewBoxRef.current = null;
      setLocalInitialDrawParams(null);
    }, [isToolActive]);

    useEffect(() => {
      const clearTouchInteraction = () => {
        setIsMaskTouchInteracting(false);
      };

      window.addEventListener('touchend', clearTouchInteraction);
      window.addEventListener('touchcancel', clearTouchInteraction);

      return () => {
        window.removeEventListener('touchend', clearTouchInteraction);
        window.removeEventListener('touchcancel', clearTouchInteraction);
      };
    }, [setIsMaskTouchInteracting]);

    const sortedSubMasks = useMemo(() => {
      if (!activeContainer) {
        return [];
      }
      const activeId = isMasking ? activeMaskId : activeAiSubMaskId;
      const selectedMask = activeContainer.subMasks.find((m: SubMask) => m.id === activeId);
      const otherMasks = activeContainer.subMasks.filter((m: SubMask) => m.id !== activeId);
      return selectedMask ? [...otherMasks, selectedMask] : activeContainer.subMasks;
    }, [activeContainer, activeMaskId, activeAiSubMaskId, isMasking, isAiEditing]);

    const cloneHealMarkers = useMemo(() => {
      const markers: any[] = [];
      if (!adjustments.aiPatches && !adjustments.masks) return markers;

      const processContainers = (containers: any[], isAi: boolean) => {
        containers.forEach((container) => {
          container.subMasks.forEach((sm: SubMask) => {
            if (sm.type !== Mask.Clone && sm.type !== Mask.Heal) return;
            const lines = sm.parameters?.lines || [];
            if (lines.length === 0) return;

            let minX = Infinity,
              minY = Infinity,
              maxX = -Infinity,
              maxY = -Infinity;
            for (const line of lines) {
              for (const pt of line.points) {
                if (pt.x < minX) minX = pt.x;
                if (pt.x > maxX) maxX = pt.x;
                if (pt.y < minY) minY = pt.y;
                if (pt.y > maxY) maxY = pt.y;
              }
            }
            if (minX === Infinity) return;

            const drawingCenterX = (minX + maxX) / 2;
            const drawingCenterY = (minY + maxY) / 2;

            const sourceX = sm.parameters?.sourceX;
            const sourceY = sm.parameters?.sourceY;

            let cx = drawingCenterX;
            let cy = drawingCenterY;

            if (sourceX !== undefined && sourceY !== undefined) {
              cx = (drawingCenterX + sourceX) / 2;
              cy = (drawingCenterY + sourceY) / 2;
            }

            markers.push({
              id: sm.id,
              containerId: container.id,
              type: sm.type,
              cx,
              cy,
              isAi,
            });
          });
        });
      };

      if (isAiEditing && adjustments.aiPatches) processContainers(adjustments.aiPatches, true);
      if (isMasking && adjustments.masks) processContainers(adjustments.masks, false);

      return markers;
    }, [adjustments, isAiEditing, isMasking]);

    useEffect(() => {
      if (isCropping && uncroppedAdjustedPreviewUrl) {
        const timer = setTimeout(() => setIsCropViewVisible(true), 10);
        return () => clearTimeout(timer);
      } else {
        setIsCropViewVisible(false);
      }
    }, [isCropping, uncroppedAdjustedPreviewUrl]);

    const handleWbClick = useCallback(
      (e: any) => {
        const sampleUrl = selectedImage?.thumbnailUrl || finalPreviewUrl;
        if (!isWbPickerActive || !sampleUrl || !onWbPicked) return;

        const stage = e.target.getStage();
        const pointerPos = getCanvasPointer(stage);
        if (!pointerPos) return;

        const x = pointerPos.x / imageRenderSize.scale;
        const y = pointerPos.y / imageRenderSize.scale;

        const imgLogicalWidth = imageRenderSize.width / imageRenderSize.scale;
        const imgLogicalHeight = imageRenderSize.height / imageRenderSize.scale;

        if (x < 0 || x > imgLogicalWidth || y < 0 || y > imgLogicalHeight) return;

        const img = new Image();
        img.crossOrigin = 'Anonymous';
        img.src = sampleUrl;

        img.onload = () => {
          const radius = 5;
          const side = radius * 2 + 1;

          const canvas = document.createElement('canvas');
          canvas.width = side;
          canvas.height = side;
          const ctx = canvas.getContext('2d', { willReadFrequently: true });
          if (!ctx) return;

          const scaleX = img.width / imgLogicalWidth;
          const scaleY = img.height / imgLogicalHeight;
          const srcX = Math.floor(x * scaleX);
          const srcY = Math.floor(y * scaleY);

          const startX = Math.max(0, srcX - radius);
          const startY = Math.max(0, srcY - radius);
          const endX = Math.min(img.width, srcX + radius + 1);
          const endY = Math.min(img.height, srcY + radius + 1);
          const sw = endX - startX;
          const sh = endY - startY;

          if (sw <= 0 || sh <= 0) return;

          ctx.drawImage(img, startX, startY, sw, sh, 0, 0, sw, sh);

          const imageData = ctx.getImageData(0, 0, sw, sh);
          const data = imageData.data;

          let rTotal = 0,
            gTotal = 0,
            bTotal = 0;
          let count = 0;

          for (let i = 0; i < data.length; i += 4) {
            rTotal += data[i];
            gTotal += data[i + 1];
            bTotal += data[i + 2];
            count++;
          }

          if (count === 0) return;

          const avgR = rTotal / count;
          const avgG = gTotal / count;
          const avgB = bTotal / count;

          const linR = Math.pow(avgR / 255.0, 2.2);
          const linG = Math.pow(avgG / 255.0, 2.2);
          const linB = Math.pow(avgB / 255.0, 2.2);

          const sumRB = linR + linB;
          const deltaTemp = sumRB > 0.0001 ? ((linB - linR) / sumRB) * 125.0 : 0;

          const linM = sumRB / 2.0;
          const sumGM = linG + linM;
          const deltaTint = sumGM > 0.0001 ? ((linG - linM) / sumGM) * 400.0 : 0;

          // ============== BLITZRAW: pick a real white balance ==============
          // The averaging above reads the preview, which has been through
          // exposure and tone mapping, so its numbers say little about the
          // light in the room. Ask the backend instead: it reads the as-shot
          // render, recovers camera RGB and solves for the illuminant that
          // calls that pixel neutral. The preview maths stays as the fallback
          // for files with no calibration, such as JPEGs.
          invoke<{ kelvin: number; tint: number }>('pick_white_balance', {
            u: x / imgLogicalWidth,
            v: y / imgLogicalHeight,
            adjustments,
          })
            .then((picked) => {
              setAdjustments((prev: Adjustments) => ({
                ...prev,
                whiteBalance: {
                  // Snapped to the grid the slider moves on, so a picked value
                  // and a dragged one are the same kind of number.
                  kelvin: Math.round(picked.kelvin / KELVIN_STEP) * KELVIN_STEP,
                  tint: Math.round(picked.tint),
                },
              }));
            })
            .catch(() => {
              setAdjustments((prev: Adjustments) => ({
                ...prev,
                temperature: Math.max(-100, Math.min(100, deltaTemp)),
                tint: Math.max(-100, Math.min(100, deltaTint)),
              }));
            })
            .finally(() => {
              onWbPicked();
            });
          return;
          // ============ BLITZRAW END: pick a real white balance ============
        };
      },
      [
        isWbPickerActive,
        selectedImage?.thumbnailUrl,
        finalPreviewUrl,
        imageRenderSize,
        onWbPicked,
        adjustments, // BLITZRAW: the pick maps through crop and rotation
        setAdjustments,
        getCanvasPointer,
      ],
    );

    const handleStart = useCallback(
      (e: any) => {
        if (e.evt && typeof e.evt.button === 'number' && e.evt.button !== 0) {
          return;
        }

        if (e.evt && e.evt.cancelable) e.evt.preventDefault();

        if (isWbPickerActive) {
          handleWbClick(e);
          return;
        }

        if (isParametricActive && activeSubMask) {
          const pos = getCanvasPointer(e.target.getStage());
          if (!pos) return;

          const { scale } = imageRenderSize;
          const x = pos.x / scale + cropX;
          const y = pos.y / scale + cropY;

          let newParams = { ...activeSubMask.parameters };
          newParams.targetX = x;
          newParams.targetY = y;
          newParams.rotation = adjustments.rotation || 0;
          newParams.flipHorizontal = adjustments.flipHorizontal || false;
          newParams.flipVertical = adjustments.flipVertical || false;
          newParams.orientationSteps = adjustments.orientationSteps || 0;
          delete newParams.isInitialDraw;

          const activeId = isMasking ? activeMaskId : activeAiSubMaskId;
          updateSubMask(activeId, { parameters: newParams });
          return;
        }

        // ============== BLITZRAW: a click places a pen anchor ==============
        // Press, and a corner is placed. Press and drag, and the same press
        // pulls handles out of it, so a corner and a curve are the same
        // gesture told apart by whether the mouse moved. That is the pen tool
        // everyone already knows from Illustrator, and it is the reason this
        // has to live in the press rather than in a separate mode.
        //
        // Landing on the first anchor closes the path instead, which is the
        // ordinary way a path ends.
        if (isPenPlacing && activeSubMask) {
          const stage = e.target.getStage();
          const pos = getCanvasPointer(stage);
          if (!pos) return;

          const { scale } = imageRenderSize;
          const x = pos.x / scale + cropX;
          const y = pos.y / scale + cropY;

          const activeId = isMasking ? activeMaskId : activeAiSubMaskId;
          const points: Array<PenAnchor> = [...(activeSubMask.parameters?.points || [])];

          if (closesPath(points, x, y, CLOSE_HIT_RADIUS / (scale * transformState.scale))) {
            updateSubMask(activeId, {
              parameters: { ...activeSubMask.parameters, points, closed: true, isDrawing: false },
            });
            setPenCursor(null);
            penDrag.current = null;
            return;
          }

          points.push({ x, y });
          penDrag.current = { index: points.length - 1, start: { x, y } };
          isDrawing.current = true;
          drawingStageRef.current = stage;

          updateSubMask(activeId, {
            parameters: { ...activeSubMask.parameters, points, isDrawing: true },
          });
          return;
        }
        // ============ BLITZRAW END: a click places a pen anchor ============

        if (isInitialDrawing && activeSubMask) {
          isDrawing.current = true;
          drawingStageRef.current = e.target.getStage();
          const pos = getCanvasPointer(e.target.getStage());
          if (!pos) return;

          const { scale } = imageRenderSize;
          const x = pos.x / scale + cropX;
          const y = pos.y / scale + cropY;

          dragStartPointer.current = { x, y };

          let initialParams = { ...activeSubMask.parameters };

          if (activeSubMask.type === Mask.Radial) {
            initialParams = {
              ...initialParams,
              centerX: x,
              centerY: y,
              radiusX: 0,
              radiusY: 0,
              rotation: 0,
            };
          } else if (activeSubMask.type === Mask.Linear) {
            initialParams = {
              ...initialParams,
              startX: x,
              startY: y,
              endX: x,
              endY: y,
              range: 0,
            };
          }

          setLocalInitialDrawParams(initialParams);
          return;
        }

        if (isManualCleanupActive && activeSubMask) {
          const isCtrlPressedLocal = e.evt.ctrlKey || e.evt.metaKey || (window as any).ctrlKeyDown;
          if (isCtrlPressedLocal || activeSubMask.parameters?.sourceX === undefined) {
            const pos = getCanvasPointer(e.target.getStage());
            if (!pos) return;

            const { scale } = imageRenderSize;
            const x = pos.x / scale + cropX;
            const y = pos.y / scale + cropY;

            const activeId = activeAiSubMaskId;
            if (activeId) {
              updateSubMask(activeId, {
                parameters: { ...activeSubMask.parameters, sourceX: x, sourceY: y },
              });

              if (onManualCleanup && activeSubMask.parameters?.lines?.length > 0) {
                onManualCleanup(activeId, x, y);
              }
            }

            if (e.evt && e.evt.cancelable) e.evt.preventDefault();
            return;
          }
        }

        if (isToolActive) {
          const stage = e.target.getStage();
          const pos = getCanvasPointer(stage);
          if (!pos) {
            isDrawing.current = false;
            currentLine.current = null;
            setPreviewBox(null);
            previewBoxRef.current = null;
            return;
          }

          if (isAiSubjectActive) {
            isDrawing.current = true;
            drawingStageRef.current = stage;
            const newBox = { start: pos, end: pos };
            previewBoxRef.current = newBox;
            setPreviewBox(newBox);
            return;
          }

          const isAltPressed = e.evt.altKey || (window as any).altKeyDown;
          let effectiveTool;

          if (isAiSubjectActive) {
            effectiveTool = ToolType.AiSeletor;
          } else if (isAltPressed) {
            effectiveTool = baseTool === ToolType.Brush ? ToolType.Eraser : ToolType.Brush;
          } else {
            effectiveTool = baseTool;
          }
          const isShiftClick = isBrushActive && e.evt.shiftKey && lastBrushPoint.current;

          if (isShiftClick) {
            const { scale } = imageRenderSize;
            const startImageSpace = lastBrushPoint.current!;
            const endImageSpace = {
              x: pos.x / scale + cropX,
              y: pos.y / scale + cropY,
            };

            const dx = endImageSpace.x - startImageSpace.x;
            const dy = endImageSpace.y - startImageSpace.y;
            const distance = Math.sqrt(dx * dx + dy * dy);
            const steps = Math.max(Math.ceil(distance), 2);
            const interpolatedPoints: Coord[] = [];
            for (let i = 0; i <= steps; i++) {
              const t = i / steps;
              interpolatedPoints.push({
                x: startImageSpace.x + dx * t,
                y: startImageSpace.y + dy * t,
              });
            }

            const imageSpaceLine: DrawnLine = {
              brushSize: brushImageSpaceSize,
              feather: brushSettings?.feather ? brushSettings?.feather / 100 : 0,
              flow: activeLineFlow,
              points: interpolatedPoints,
              tool: effectiveTool,
            };

            const activeId = isMasking ? activeMaskId : activeAiSubMaskId;
            const existingLines = activeSubMask?.parameters?.lines || [];

            updateSubMask(activeId, {
              parameters: {
                ...activeSubMask?.parameters,
                lines: [...existingLines, imageSpaceLine],
              },
            });

            lastBrushPoint.current = endImageSpace;
            isDrawing.current = false;
            currentLine.current = null;
            return;
          }

          isDrawing.current = true;
          activeStrokeIndex.current = null;
          drawingStageRef.current = stage;

          if (isManualCleanupActive) {
          }

          const newLine: DrawnLine = {
            brushSize: isBrushActive && brushSettings?.size ? brushStageSize : 2,
            points: [pos],
            tool: effectiveTool,
          };
          currentLine.current = newLine;
        } else {
          if (e.target === e.target.getStage()) {
            if (isMasking) {
              onSelectMask(null);
            }
            if (isAiEditing) {
              onSelectAiSubMask(null);
            }
          }
        }
      },
      [
        isWbPickerActive,
        handleWbClick,
        isInitialDrawing,
        isBrushActive,
        isManualCleanupActive,
        activeLineFlow,
        isAiSubjectActive,
        isParametricActive,
        brushSettings,
        onSelectMask,
        onSelectAiSubMask,
        isMasking,
        isAiEditing,
        imageRenderSize,
        adjustments,
        activeMaskId,
        activeAiSubMaskId,
        activeSubMask,
        updateSubMask,
        cropX,
        cropY,
        isToolActive,
        brushImageSpaceSize,
        brushStageSize,
        baseTool,
        getCanvasPointer,
      ],
    );

    // ============ BLITZRAW: the eyedropper lets go of the photo ============
    // Has the pointer been over the photo since the eyedropper was picked up?
    //
    // The tool is turned on by a button in a side panel, and the way from that
    // button to the photo crosses the padding around it. Letting go the moment
    // the pointer was not over the photo therefore put the tool down on the way
    // to using it, every time, before it had ever been over the picture. So
    // leaving only counts once there has been an arriving.
    const wbPickerReachedImage = useRef(false);

    useEffect(() => {
      wbPickerReachedImage.current = false;
    }, [isWbPickerActive]);

    // Whether the pointer is over the picture itself, rather than over the
    // stage, which is the picture plus half its width of padding on each side.
    // The same bounds `handleWbClick` uses to decide a pick is valid, so being
    // outside them already meant a click would do nothing.
    const isPointerOverImage = useCallback(
      (stage: any) => {
        const pointerPos = getCanvasPointer(stage);
        if (!pointerPos || !imageRenderSize.scale) {
          return false;
        }
        const x = pointerPos.x / imageRenderSize.scale;
        const y = pointerPos.y / imageRenderSize.scale;
        const width = imageRenderSize.width / imageRenderSize.scale;
        const height = imageRenderSize.height / imageRenderSize.scale;
        return x >= 0 && x <= width && y >= 0 && y <= height;
      },
      [getCanvasPointer, imageRenderSize],
    );
    // ========== BLITZRAW END: the eyedropper lets go of the photo ==========

    const handleMove = useCallback(
      (e: any) => {
        if (isWbPickerActive) {
          // BLITZRAW: leaving the photo puts the eyedropper down. Not the pick
          // itself: finding a neutral takes several tries on different parts of
          // the picture, and a one-shot tool would have to be picked up again
          // between every one of them. Leaving is the moment somebody has
          // finished, and it is also the moment they stop being able to see
          // that the tool is still in their hand.
          //
          // Arriving has to come first. Without that, the walk from the button
          // in the panel to the photo counts as a leaving and the tool is put
          // down before it reaches the picture.
          if (e && typeof e.target?.getStage === 'function') {
            const stage = e.target.getStage();
            if (stage) {
              if (isPointerOverImage(stage)) {
                wbPickerReachedImage.current = true;
              } else if (wbPickerReachedImage.current) {
                dismissWbPicker();
              }
            }
          }
          return;
        }

        let pos;
        if (e && typeof e.target?.getStage === 'function') {
          const stage = e.target.getStage();
          pos = getCanvasPointer(stage);
        } else if (e && (e.clientX != null || (e.touches && e.touches[0]))) {
          const stage = drawingStageRef.current;
          if (stage) {
            stage.setPointersPositions(e);
            pos = getCanvasPointer(stage);
          }
        }

        if (isToolActive) {
          if (pos) {
            setCursorPreview({ x: pos.x, y: pos.y, visible: true });
          } else {
            setCursorPreview((p: CursorPreview) => ({ ...p, visible: false }));
          }
        }

        // ======== BLITZRAW: the pen follows the cursor even when idle ========
        // The rubber band from the last anchor has to track the mouse with no
        // button down, so this runs before the drawing guard below. The guard
        // exists for tools that only do something mid-stroke; the pen shows
        // where the next segment would go before anything is committed.
        if (isPenPlacing && pos) {
          setPenCursor(pos);

          if (penDrag.current && activeSubMask) {
            const { scale } = imageRenderSize;
            const x = pos.x / scale + cropX;
            const y = pos.y / scale + cropY;
            const { index, start } = penDrag.current;

            // A press that has not really moved is a corner, so nothing is
            // written until the drag is past the threshold. Without this every
            // plain click would leave two handles sitting on top of its anchor
            // and the next segment would be a curve nobody asked for.
            if (Math.hypot(x - start.x, y - start.y) * scale * transformState.scale < HANDLE_DRAG_THRESHOLD) {
              return;
            }

            const points: Array<PenAnchor> = [...(activeSubMask.parameters?.points || [])];
            if (points[index]) {
              points[index] = {
                ...points[index],
                handleOut: { x, y },
                handleIn: mirrorHandle(points[index], { x, y }),
              };
              const activeId = isMasking ? activeMaskId : activeAiSubMaskId;
              updateSubMask(activeId, { parameters: { ...activeSubMask.parameters, points } });
            }
          }
          return;
        }
        // ====== BLITZRAW END: the pen follows the cursor even when idle ======

        if (!isDrawing.current || !isToolActive) {
          return;
        }

        if (isAiSubjectActive && previewBoxRef.current && pos) {
          const updatedBox = { ...previewBoxRef.current, end: pos };
          previewBoxRef.current = updatedBox;
          setPreviewBox(updatedBox);
          if (e.evt && e.evt.cancelable) e.evt.preventDefault();
          return;
        }

        if (isInitialDrawing && dragStartPointer.current && activeSubMask && localInitialDrawParams) {
          const stage =
            drawingStageRef.current || (e && typeof e.target?.getStage === 'function' ? e.target.getStage() : null);
          if (!stage) return;
          const pointerPos = getCanvasPointer(stage);
          if (!pointerPos) return;

          const { scale } = imageRenderSize;
          const x = pointerPos.x / scale + cropX;
          const y = pointerPos.y / scale + cropY;

          const distX = x - dragStartPointer.current.x;
          const distY = y - dragStartPointer.current.y;
          const screenThreshold = 15;
          if (Math.sqrt(distX * distX + distY * distY) < screenThreshold / scale) {
            return;
          }

          let updatedParams = { ...localInitialDrawParams };

          if (activeSubMask.type === Mask.Radial) {
            updatedParams.radiusX = Math.max(1, Math.abs(x - dragStartPointer.current.x));
            updatedParams.radiusY = Math.max(1, Math.abs(y - dragStartPointer.current.y));
          } else if (activeSubMask.type === Mask.Linear) {
            // BLITZRAW: a gradient begins where I press, and has faded to
            // nothing where I let go. It used to put the press on the 50% mark,
            // so the effect reached full strength a whole drag-length behind
            // where I started. See utils/linearMask.ts.
            const handleDist =
              Math.min(effectiveImageDimensions.width, effectiveImageDimensions.height) * 0.2;
            updatedParams = {
              ...updatedParams,
              ...linearFromDrag(dragStartPointer.current.x, dragStartPointer.current.y, x, y, handleDist),
            };
          }

          setLocalInitialDrawParams(updatedParams);

          if (onLiveMaskPreview && activeContainer && activeSubMask) {
            const previewSubMask = {
              ...activeSubMask,
              parameters: updatedParams,
            };
            const previewContainer = {
              ...activeContainer,
              subMasks: activeContainer.subMasks.map((sm: SubMask) =>
                sm.id === activeSubMask.id ? previewSubMask : sm,
              ),
            };
            onLiveMaskPreview(previewContainer);
          }

          const activeId = isMasking ? activeMaskId : activeAiSubMaskId;
          if (activeId) {
            updateSubMask(activeId, { parameters: updatedParams });
          }

          if (e.evt && e.evt.cancelable) e.evt.preventDefault();
          return;
        }

        if (!pos) {
          return;
        }

        if (currentLine.current) {
          const lastPoint = currentLine.current.points[currentLine.current.points.length - 1];
          if (lastPoint) {
            const dx = pos.x - lastPoint.x;
            const dy = pos.y - lastPoint.y;
            if (dx * dx + dy * dy < 4) {
              if (e.evt && e.evt.cancelable) e.evt.preventDefault();
              return;
            }
          }

          const updatedLine = {
            ...currentLine.current,
            points: [...currentLine.current.points, pos],
          };
          currentLine.current = updatedLine;

          const activeId = isMasking ? activeMaskId : activeAiSubMaskId;

          if (isManualCleanupActive && activeId) {
            const { scale } = imageRenderSize;

            const imageSpaceLine: DrawnLine = {
              brushSize: brushImageSpaceSize,
              feather: brushSettings?.feather ? brushSettings?.feather / 100 : 0,
              flow: activeLineFlow,
              points: updatedLine.points.map((p: Coord) => ({
                x: p.x / scale + cropX,
                y: p.y / scale + cropY,
              })),
              tool: updatedLine.tool,
            };

            const existingLines = activeSubMask?.parameters?.lines ? [...activeSubMask.parameters.lines] : [];

            if (activeStrokeIndex.current !== null) {
              existingLines[activeStrokeIndex.current] = imageSpaceLine;
            } else {
              activeStrokeIndex.current = existingLines.length;
              existingLines.push(imageSpaceLine);
            }

            updateSubMask(activeId, {
              parameters: {
                ...activeSubMask?.parameters,
                lines: existingLines,
              },
            });

            const sourceX = activeSubMask?.parameters.sourceX;
            const sourceY = activeSubMask?.parameters.sourceY;
            if (sourceX !== undefined && sourceY !== undefined) {
              triggerManualCleanup(activeId, sourceX, sourceY);
            }
          } else if (onLiveMaskPreview && activeContainer && activeSubMask && isBrushActive) {
            const { scale } = imageRenderSize;

            const imageSpaceLine: DrawnLine = {
              brushSize: brushImageSpaceSize,
              feather: brushSettings?.feather ? brushSettings?.feather / 100 : 0,
              flow: activeLineFlow,
              points: updatedLine.points.map((p: Coord) => ({
                x: p.x / scale + cropX,
                y: p.y / scale + cropY,
              })),
              tool: updatedLine.tool,
            };

            const existingLines = activeSubMask.parameters?.lines || [];
            const previewSubMask = {
              ...activeSubMask,
              parameters: {
                ...activeSubMask.parameters,
                lines: [...existingLines, imageSpaceLine],
              },
            };

            const previewContainer = {
              ...activeContainer,
              subMasks: activeContainer.subMasks.map((sm: SubMask) =>
                sm.id === activeSubMask.id ? previewSubMask : sm,
              ),
            };

            onLiveMaskPreview(previewContainer);
          }
          if (e.evt && e.evt.cancelable) e.evt.preventDefault();
        }
      },
      [
        isToolActive,
        isWbPickerActive,
        isInitialDrawing,
        activeMaskId,
        activeAiSubMaskId,
        updateSubMask,
        onLiveMaskPreview,
        activeContainer,
        activeSubMask,
        isBrushActive,
        isManualCleanupActive,
        onManualCleanup,
        activeLineFlow,
        isAiSubjectActive,
        imageRenderSize,
        cropX,
        cropY,
        effectiveImageDimensions,
        brushSettings,
        isMasking,
        localInitialDrawParams,
        brushImageSpaceSize,
        baseTool,
        getCanvasPointer,
        isPointerOverImage,
      ],
    );

    const handleUp = useCallback(() => {
      // BLITZRAW: letting go ends the handle pull, not the path. The path is
      // still being placed and the next press adds the next anchor, so this
      // returns rather than falling through to the endings below.
      if (isPenPlacing) {
        penDrag.current = null;
        isDrawing.current = false;
        return;
      }

      if (!isDrawing.current) {
        return;
      }

      if (isInitialDrawing && activeSubMask) {
        isDrawing.current = false;
        const activeId = isMasking ? activeMaskId : activeAiSubMaskId;

        const newParams = { ...localInitialDrawParams };
        delete newParams.isInitialDraw;

        if (activeSubMask.type === Mask.Radial && newParams.radiusX < 10 && newParams.radiusY < 10) {
          newParams.radiusX = 100;
          newParams.radiusY = 100;
        } else if (activeSubMask.type === Mask.Linear) {
          if (!newParams.range || newParams.range < 10) {
            const handleDist = Math.min(effectiveImageDimensions.width, effectiveImageDimensions.height) * 0.2;
            newParams.startX = dragStartPointer.current!.x + handleDist;
            newParams.startY = dragStartPointer.current!.y;
            newParams.endX = dragStartPointer.current!.x - handleDist;
            newParams.endY = dragStartPointer.current!.y;
            newParams.range = 100;
          }
        }

        updateSubMask(activeId, { parameters: newParams });
        setLocalInitialDrawParams(null);
        dragStartPointer.current = null;
        return;
      }

      if (!currentLine.current && !(isAiSubjectActive && previewBoxRef.current)) {
        return;
      }

      if (isAiSubjectActive && previewBoxRef.current) {
        const wasDrawing = isDrawing.current;
        isDrawing.current = false;
        const box = previewBoxRef.current;
        previewBoxRef.current = null;
        setPreviewBox(null);
        drawingStageRef.current = null;

        if (!wasDrawing || !box) {
          return;
        }

        const { scale } = imageRenderSize;
        const activeId = isMasking ? activeMaskId : activeAiSubMaskId;

        let startPoint = { x: box.start.x / scale + cropX, y: box.start.y / scale + cropY };
        let endPoint = { x: box.end.x / scale + cropX, y: box.end.y / scale + cropY };

        const dx = box.end.x - box.start.x;
        const dy = box.end.y - box.start.y;
        if (Math.sqrt(dx * dx + dy * dy) < 5) {
          endPoint = { x: startPoint.x, y: startPoint.y };
        }

        if (activeId) {
          updateSubMask(activeId, {
            parameters: {
              ...activeSubMask?.parameters,
              startX: startPoint.x,
              startY: startPoint.y,
              endX: endPoint.x,
              endY: endPoint.y,
            },
          });
        }

        if (activeSubMask?.type === Mask.QuickEraser && onQuickErase) {
          onQuickErase(activeId, startPoint, endPoint);
        } else if (activeSubMask?.type === Mask.AiSubject && onGenerateAiMask) {
          onGenerateAiMask(activeId, startPoint, endPoint);
        }
        return;
      }

      const wasDrawing = isDrawing.current;
      isDrawing.current = false;
      const line = currentLine.current;
      currentLine.current = null;
      drawingStageRef.current = null;

      if (!wasDrawing || !line) {
        return;
      }

      const { scale } = imageRenderSize;
      const activeId = isMasking ? activeMaskId : activeAiSubMaskId;

      if (isBrushActive) {
        const imageSpaceLine: DrawnLine = {
          brushSize: brushImageSpaceSize,
          feather: brushSettings?.feather ? brushSettings?.feather / 100 : 0,
          flow: activeLineFlow,
          points: line.points.map((p: Coord) => ({
            x: p.x / scale + cropX,
            y: p.y / scale + cropY,
          })),
          tool: line.tool,
        };

        const existingLines = activeSubMask?.parameters?.lines ? [...activeSubMask.parameters.lines] : [];

        if (activeStrokeIndex.current !== null) {
          existingLines[activeStrokeIndex.current] = imageSpaceLine;
        } else {
          existingLines.push(imageSpaceLine);
        }

        updateSubMask(activeId, {
          parameters: {
            ...activeSubMask?.parameters,
            lines: existingLines,
          },
        });

        activeStrokeIndex.current = null;

        const lastPoint = line.points[line.points.length - 1];
        if (lastPoint) {
          lastBrushPoint.current = {
            x: lastPoint.x / scale + cropX,
            y: lastPoint.y / scale + cropY,
          };
        }

        if (isManualCleanupActive && activeId) {
          const sourceX = activeSubMask?.parameters.sourceX;
          const sourceY = activeSubMask?.parameters.sourceY;
          if (sourceX !== undefined && sourceY !== undefined) {
            triggerManualCleanup(activeId, sourceX, sourceY);
          }
        }
      }
    }, [
      isInitialDrawing,
      activeAiSubMaskId,
      activeMaskId,
      activeSubMask,
      cropX,
      cropY,
      brushSettings,
      imageRenderSize.scale,
      isAiEditing,
      isBrushActive,
      isManualCleanupActive,
      triggerManualCleanup,
      activeLineFlow,
      isMasking,
      onGenerateAiMask,
      onQuickErase,
      updateSubMask,
      effectiveImageDimensions,
      localInitialDrawParams,
      brushImageSpaceSize,
      brushStageSize,
      baseTool,
    ]);

    const handleMouseEnter = useCallback(() => {
      if (isToolActive) {
        setCursorPreview((p: CursorPreview) => ({ ...p, visible: true }));
      }
    }, [isToolActive]);

    const handleMouseLeave = useCallback(() => {
      setCursorPreview((p: CursorPreview) => ({ ...p, visible: false }));
      // BLITZRAW: a pointer moved off the photo fast enough can leave the whole
      // stage without a single move event landing in the padding, and then
      // nothing above would have noticed. Same rule: only once it has been
      // over the photo, or crossing the padding on the way in would end it.
      if (wbPickerReachedImage.current) {
        dismissWbPicker();
      }
    }, []);

    useEffect(() => {
      if (!isToolActive) return;

      function onGlobalMove(e: MouseEvent | TouchEvent) {
        if (!isDrawing.current) return;
        handleMove(e);
      }

      function onGlobalUp() {
        if (!isDrawing.current) return;
        handleUp();
      }

      window.addEventListener('mousemove', onGlobalMove, { passive: false });
      window.addEventListener('mouseup', onGlobalUp);
      window.addEventListener('touchmove', onGlobalMove, { passive: false });
      window.addEventListener('touchcancel', onGlobalUp);
      return () => {
        window.removeEventListener('mousemove', onGlobalMove);
        window.removeEventListener('mouseup', onGlobalUp);
        window.removeEventListener('touchmove', onGlobalMove);
        window.removeEventListener('touchcancel', onGlobalUp);
      };
    }, [isToolActive, handleMove, handleUp]);

    const handleStraightenMouseDown = (e: any) => {
      if (e.evt.button !== 0 && !e.evt.touches) {
        return;
      }

      isStraightening.current = true;
      const pos = e.target.getStage().getPointerPosition();
      setStraightenLine({ start: pos, end: pos });
    };

    const handleStraightenMouseMove = (e: any) => {
      if (!isStraightening.current) {
        return;
      }

      const pos = e.target.getStage().getPointerPosition();
      setStraightenLine((prev: any) => ({ ...prev, end: pos }));
      if (e.evt && e.evt.cancelable) e.evt.preventDefault();
    };

    const handleStraightenMouseUp = () => {
      if (!isStraightening.current) {
        return;
      }
      isStraightening.current = false;
      if (
        !straightenLine ||
        (straightenLine.start.x === straightenLine.end.x && straightenLine.start.y === straightenLine.start.y)
      ) {
        setStraightenLine(null);
        return;
      }

      const { start, end } = straightenLine;
      const { rotation = 0 } = adjustments;
      const theta_rad = (rotation * Math.PI) / 180;
      const cos_t = Math.cos(theta_rad);
      const sin_t = Math.sin(theta_rad);
      const width = uncroppedImageRenderSize?.width ?? 0;
      const height = uncroppedImageRenderSize?.height ?? 0;
      const cx = width / 2;
      const cy = height / 2;

      const unrotate = (p: Coord) => {
        const x = p.x - cx;
        const y = p.y - cy;
        return {
          x: cx + x * cos_t + y * sin_t,
          y: cy - x * sin_t + y * cos_t,
        };
      };

      const start_unrotated = unrotate(start);
      const end_unrotated = unrotate(end);
      const dx = end_unrotated.x - start_unrotated.x;
      const dy = end_unrotated.y - start_unrotated.y;
      const angle = Math.atan2(dy, dx) * (180 / Math.PI);
      let targetAngle;

      if (angle > -45 && angle <= 45) {
        targetAngle = 0;
      } else if (angle > 45 && angle <= 135) {
        targetAngle = 90;
      } else if (angle > 135 || angle <= -135) {
        targetAngle = 180;
      } else {
        targetAngle = -90;
      }

      let correction = targetAngle - angle;
      if (correction > 180) {
        correction -= 360;
      }
      if (correction < -180) {
        correction += 360;
      }

      onStraighten(correction);
      setStraightenLine(null);
    };

    const handleStraightenMouseLeave = () => {
      if (isStraightening.current) {
        isStraightening.current = false;
        setStraightenLine(null);
      }
    };

    const cropPreviewUrl = uncroppedAdjustedPreviewUrl || selectedImage.thumbnailUrl;
    const originalSrc = transformedOriginalUrl;
    const isShowingOriginal = showOriginal && !!originalSrc;

    useEffect(() => {
      if (!originalSrc) {
        setOriginalLoaded(false);
        return;
      }

      const img = new Image();
      img.src = originalSrc;

      if (img.complete) {
        setOriginalLoaded(true);
      } else {
        setOriginalLoaded(false);
        img.onload = () => setOriginalLoaded(true);
      }

      return () => {
        img.onload = null;
      };
    }, [originalSrc]);

    const currentTarget = finalPreviewUrl || cachedPreviewUrl || selectedImage.thumbnailUrl;
    const baseIsReady = displayState.base === currentTarget && !displayState.fade;

    const visiblePatch = interactivePatch ?? (baseIsReady ? null : retainedPatchRef.current);

    useEffect(() => {
      if (baseIsReady && !interactivePatch) {
        retainedPatchRef.current = null;
      }
    }, [baseIsReady, interactivePatch]);

    const uncroppedImageRenderSize = useMemo<Partial<RenderSize> | null>(() => {
      if (!selectedImage?.width || !selectedImage?.height || !imageRenderSize?.width || !imageRenderSize?.height) {
        return null;
      }

      const viewportWidth = imageRenderSize.width + 2 * imageRenderSize.offsetX;
      const viewportHeight = imageRenderSize.height + 2 * imageRenderSize.offsetY;

      let uncroppedEffectiveWidth = selectedImage.width;
      let uncroppedEffectiveHeight = selectedImage.height;
      const orientationSteps = adjustments.orientationSteps || 0;
      if (orientationSteps === 1 || orientationSteps === 3) {
        [uncroppedEffectiveWidth, uncroppedEffectiveHeight] = [uncroppedEffectiveHeight, uncroppedEffectiveWidth];
      }

      if (uncroppedEffectiveWidth <= 0 || uncroppedEffectiveHeight <= 0 || viewportWidth <= 0 || viewportHeight <= 0) {
        return null;
      }

      const scale = Math.min(viewportWidth / uncroppedEffectiveWidth, viewportHeight / uncroppedEffectiveHeight);

      const renderWidth = uncroppedEffectiveWidth * scale;
      const renderHeight = uncroppedEffectiveHeight * scale;

      return { width: renderWidth, height: renderHeight };
    }, [selectedImage?.width, selectedImage?.height, imageRenderSize, adjustments.orientationSteps]);

    const cropImageTransforms = useMemo(() => {
      const rotation = liveRotation !== null && liveRotation !== undefined ? liveRotation : adjustments.rotation || 0;
      return `rotate(${rotation}deg)`;
    }, [adjustments.rotation, liveRotation]);

    // ============ BLITZRAW: rotate from the crop corners ============
    // The same three steps the rotation slider takes, so the two controls are
    // interchangeable: announce the drag, preview through `liveRotation`, then
    // commit once at the end. Committing on every move would push a history
    // step per frame and re-render the photo at full size each time.
    const shownRotation =
      liveRotation !== null && liveRotation !== undefined ? liveRotation : adjustments.rotation || 0;

    const handleCornerRotateStart = useCallback(() => {
      useEditorStore.getState().setEditor({ isRotationActive: true });
    }, []);

    const handleCornerRotate = useCallback((degrees: number) => {
      useEditorStore.getState().setEditor({ liveRotation: degrees });
    }, []);

    const handleCornerRotateEnd = useCallback(
      (degrees: number) => {
        useEditorStore.getState().setEditor({ liveRotation: null, isRotationActive: false });
        setAdjustments((prev: Adjustments) => ({ ...prev, rotation: degrees }));
      },
      [setAdjustments],
    );
    // ========== BLITZRAW END: rotate from the crop corners ==========

    // ============ BLITZRAW: resize a locked crop from its edges ============
    // react-image-crop hides its own edge handles as soon as an aspect ratio is
    // set, leaving only corners. These put the edges back, resizing about the
    // opposite edge and the centre of the other axis. See utils/cropSides.ts.
    const handleSideResize = useCallback(
      (next: { x: number; y: number; width: number; height: number }) => {
        const percent: PercentCrop = { unit: '%', ...next };
        // Through the same call react-image-crop makes, so every rule about
        // what a crop may be applies to these the same as to the corners.
        setCrop(percent as unknown as Crop, percent);
      },
      [setCrop],
    );

    const handleSideResizeEnd = useCallback(
      (next: { x: number; y: number; width: number; height: number }) => {
        const percent: PercentCrop = { unit: '%', ...next };
        handleCropComplete?.(percent as unknown as Crop, percent);
      },
      [handleCropComplete],
    );
    // ========== BLITZRAW END: resize a locked crop from its edges ==========

    const getCropDimensions = () => {
      if (!crop || !uncroppedImageRenderSize?.width || !uncroppedImageRenderSize?.height) {
        return { width: 0, height: 0 };
      }

      const width = crop.unit === '%' ? uncroppedImageRenderSize.width * (crop.width / 100) : crop.width;
      const height = crop.unit === '%' ? uncroppedImageRenderSize.height * (crop.height / 100) : crop.height;

      return { width, height };
    };

    const effectiveCursor = useMemo(() => {
      if (isWbPickerActive) return 'crosshair';
      if (isParametricActive) return 'crosshair';
      if (isInitialDrawing) return 'crosshair';
      // BLITZRAW: the pen places a point where the cross sits, so it wants the
      // same cursor the other click-to-place tools have.
      if (isPenPlacing) return 'crosshair';

      if (isBrushActive && !isManualCleanupActive) return 'none';

      if (isManualCleanupActive) {
        if (activeSubMask?.parameters?.sourceX === undefined || isCtrlPressed) {
          const targetSvg = `<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="1.5" style="filter: drop-shadow(0px 1px 2px rgba(0,0,0,0.8));">
        <circle cx="12" cy="12" r="5" />
        <line x1="12" y1="2" x2="12" y2="10" />
        <line x1="12" y1="14" x2="12" y2="22" />
        <line x1="2" y1="12" x2="10" y2="12" />
        <line x1="14" y1="12" x2="22" y2="12" />
      </svg>`;

          return `url('data:image/svg+xml;utf8,${encodeURIComponent(targetSvg)}') 12 12, crosshair`;
        }
        return 'none';
      }

      if (isAiSubjectActive) return 'crosshair';

      return cursorStyle;
    }, [
      isWbPickerActive,
      isInitialDrawing,
      isBrushActive,
      isManualCleanupActive,
      activeSubMask,
      isAiSubjectActive,
      isParametricActive,
      cursorStyle,
      isCtrlPressed,
    ]);

    const handlePreviewUpdate = useCallback(
      (id: string, subMaskPreview: Partial<SubMask>) => {
        if (!activeContainer || !onLiveMaskPreview) return;
        const previewContainer = {
          ...activeContainer,
          subMasks: activeContainer.subMasks.map((sm: SubMask) => (sm.id === id ? { ...sm, ...subMaskPreview } : sm)),
        };
        onLiveMaskPreview(previewContainer);
      },
      [activeContainer, onLiveMaskPreview],
    );

    // ============ BLITZRAW: copying a sub-mask from the photo itself ============
    // The masks panel has had Duplicate on its context menu for a long time,
    // and it drops the copy exactly on top of the original, which then has to
    // be found and dragged off it. Alt and drag does both at once, which is the
    // gesture every drawing program already uses, and it happens where the eye
    // already is rather than in a list on the right.
    //
    // It lands in the same container as the original, right after it, so the
    // copy inherits the same adjustments and the same additive or subtractive
    // sense. A copy in a container of its own would be a new mask rather than a
    // second piece of this one.
    const duplicateSubMaskOnCanvas = useCallback(
      (source: SubMask) => {
        const container = activeContainer;
        if (!container) return;

        const copy: SubMask = { ...JSON.parse(JSON.stringify(source)), id: crypto.randomUUID() };

        setAdjustments((prev: Adjustments) => ({
          ...prev,
          masks: prev.masks.map((maskContainer: MaskContainer) => {
            if (maskContainer.id !== container.id) return maskContainer;
            const subMasks = [...maskContainer.subMasks];
            const at = subMasks.findIndex((sm: SubMask) => sm.id === source.id);
            subMasks.splice(at >= 0 ? at + 1 : subMasks.length, 0, copy);
            return { ...maskContainer, subMasks };
          }),
        }));

        // Selected, so the drag that follows the Alt press carries the copy and
        // leaves the original where it was.
        if (isMasking) {
          onSelectMask(copy.id);
        } else {
          onSelectAiSubMask(copy.id);
        }
      },
      [activeContainer, setAdjustments, isMasking, onSelectMask, onSelectAiSubMask],
    );
    // ========== BLITZRAW END: copying a sub-mask from the photo itself ==========

    const handleMaskInteractionStart = useCallback(
      (e?: any) => {
        const eventType = e?.evt?.type;
        if (eventType === 'touchstart') {
          setIsMaskTouchInteracting(true);
        }
      },
      [setIsMaskTouchInteracting],
    );

    const handleMaskInteractionEnd = useCallback(() => {
      setIsMaskTouchInteracting(false);
    }, [setIsMaskTouchInteracting]);

    const currentActiveSubMaskId = activeAiSubMaskId || activeMaskId;

    // ============ BLITZRAW: whether the red is drawn ============
    // Decided in Editor.tsx from the rule in utils/maskOverlay.ts, because it
    // needs to know what the mask is set to and this component does not. What
    // is left here is the one case that has never followed that rule: clone and
    // heal, where the overlay is how you find the two ends of a patch, so it
    // follows the marker under the pointer instead.
    const maskOpacity = isShowingOriginal
      ? 0
      : isCloneOrHealActive
        ? hoveredMarkerId === currentActiveSubMaskId || isMaskControlHovered
          ? 1
          : 0
        : isMaskOverlayWanted
          ? 1
          : 0;
    // ========== BLITZRAW END: whether the red is drawn ==========

    return (
      <div className="relative" style={{ width: '100%', height: '100%', cursor: effectiveCursor }}>
        <div
          className="absolute inset-0 w-full h-full transition-opacity duration-200 flex items-center justify-center"
          style={{
            opacity: isCropViewVisible ? 0 : 1,
            pointerEvents: isCropViewVisible ? 'none' : 'auto',
          }}
        >
          <div
            className="opacity-100"
            style={{
              height: '100%',
              position: 'relative',
              width: '100%',
            }}
          >
            <div className="absolute inset-0 w-full h-full">
              <svg
                className="pointer-events-none"
                style={
                  imageRenderSize.width > 0 && imageRenderSize.height > 0
                    ? {
                        position: 'absolute',
                        left: `${imageRenderSize.offsetX}px`,
                        top: `${imageRenderSize.offsetY}px`,
                        width: `${imageRenderSize.width}px`,
                        height: `${imageRenderSize.height}px`,
                        overflow: 'visible',
                      }
                    : {
                        position: 'absolute',
                        inset: '0px',
                        width: '100%',
                        height: '100%',
                        overflow: 'visible',
                      }
                }
              >
                {displayState.base && !isWgpuActive && (
                  <image
                    href={displayState.base}
                    x="0"
                    y="0"
                    width="100%"
                    height="100%"
                    style={{ imageRendering: isMaxZoom ? 'pixelated' : 'auto' }}
                  />
                )}

                {displayState.fade && !isWgpuActive && (
                  <image
                    href={displayState.fade}
                    x="0"
                    y="0"
                    width="100%"
                    height="100%"
                    style={{
                      imageRendering: isMaxZoom ? 'pixelated' : 'auto',
                      opacity: isFadingIn ? 1 : 0,
                      transition: 'opacity 150ms ease-in-out',
                    }}
                  />
                )}

                {visiblePatch && !isWgpuActive && (
                  <image
                    href={visiblePatch.url}
                    x={`${visiblePatch.normX * 100}%`}
                    y={`${visiblePatch.normY * 100}%`}
                    width={`${visiblePatch.normW * 100}%`}
                    height={`${visiblePatch.normH * 100}%`}
                    preserveAspectRatio="none"
                    style={{ imageRendering: isMaxZoom ? 'pixelated' : 'auto' }}
                  />
                )}
              </svg>

              {originalSrc && (
                <img
                  alt="Original"
                  className={
                    imageRenderSize.width > 0 && imageRenderSize.height > 0
                      ? 'pointer-events-none'
                      : 'absolute inset-0 w-full h-full object-contain pointer-events-none'
                  }
                  src={originalSrc}
                  style={
                    imageRenderSize.width > 0 && imageRenderSize.height > 0
                      ? {
                          position: 'absolute',
                          left: `${imageRenderSize.offsetX}px`,
                          top: `${imageRenderSize.offsetY}px`,
                          width: `${imageRenderSize.width}px`,
                          height: `${imageRenderSize.height}px`,
                          imageRendering: isMaxZoom ? 'pixelated' : 'auto',
                          opacity: isShowingOriginal && originalLoaded ? 1 : 0,
                          transition: originalLoaded ? 'opacity 150ms ease-in-out' : 'none',
                          zIndex: 2,
                        }
                      : {
                          imageRendering: isMaxZoom ? 'pixelated' : 'auto',
                          opacity: isShowingOriginal && originalLoaded ? 1 : 0,
                          transition: originalLoaded ? 'opacity 150ms ease-in-out' : 'none',
                          zIndex: 2,
                        }
                  }
                />
              )}
              {displayedMaskUrl && (
                <img
                  alt="Mask Overlay"
                  className="absolute object-contain pointer-events-none"
                  src={displayedMaskUrl}
                  style={{
                    height: `${imageRenderSize.height}px`,
                    left: `${imageRenderSize.offsetX}px`,
                    opacity: maskOpacity,
                    top: `${imageRenderSize.offsetY}px`,
                    transition: 'opacity 300ms ease-in-out',
                    width: `${imageRenderSize.width}px`,
                    imageRendering: isMaxZoom ? 'pixelated' : 'auto',
                    zIndex: 3,
                  }}
                />
              )}
            </div>

            <div className="absolute inset-0 pointer-events-none z-50">
              {!isDrawing.current &&
                cloneHealMarkers.map((m) => {
                  const left = (m.cx - cropX) * imageRenderSize.scale + imageRenderSize.offsetX;
                  const top = (m.cy - cropY) * imageRenderSize.scale + imageRenderSize.offsetY;

                  return (
                    <div
                      key={`html-marker-${m.id}`}
                      className="absolute pointer-events-auto flex items-center justify-center cursor-pointer"
                      style={{
                        left,
                        top,
                        transform: `translate(-50%, -50%) scale(${1 / maxSafeScale})`,
                        transformOrigin: 'center',
                      }}
                      onMouseEnter={() => {
                        setHoveredMarkerId(m.id);
                        setIsMaskHovered(true);
                      }}
                      onMouseLeave={() => {
                        setHoveredMarkerId(null);
                        setIsMaskHovered(false);
                      }}
                      onPointerDown={(e) => {
                        e.stopPropagation();
                      }}
                      onClick={(e) => {
                        e.stopPropagation();
                        if (m.isAi) {
                          if (onSelectAiPatchContainer) onSelectAiPatchContainer(m.containerId);
                          onSelectAiSubMask(m.id);
                        } else {
                          if (onSelectMaskContainer) onSelectMaskContainer(m.containerId);
                          onSelectMask(m.id);
                        }
                      }}
                    >
                      <div className="p-1.5 rounded-full shadow-md transition-transform hover:scale-110 bg-surface/70 text-text-primary shadow-black/20">
                        {m.type === Mask.Clone ? <Stamp size={16} /> : <Bandage size={16} />}
                      </div>
                    </div>
                  );
                })}

              {!isDrawing.current &&
                activeSubMask &&
                (activeSubMask.type === Mask.Clone || activeSubMask.type === Mask.Heal) &&
                activeSubMask.parameters?.sourceX !== undefined &&
                activeSubMask.parameters?.sourceY !== undefined && (
                  <div
                    className="absolute pointer-events-auto rounded-full"
                    style={{
                      left:
                        (activeSubMask.parameters.sourceX - cropX) * imageRenderSize.scale + imageRenderSize.offsetX,
                      top: (activeSubMask.parameters.sourceY - cropY) * imageRenderSize.scale + imageRenderSize.offsetY,
                      width: 32,
                      height: 32,
                      transform: `translate(-50%, -50%) scale(${1 / maxSafeScale})`,
                      transformOrigin: 'center',
                      cursor: 'crosshair',
                    }}
                    data-tooltip={t('editor.masks.tooltips.selectNewSourcePoint', { modifier: modifierKey })}
                  />
                )}
            </div>
          </div>

          {(isMasking || isAiEditing || isWbPickerActive) && (
            <div
              style={{
                position: 'absolute',
                top: stageTop,
                left: stageLeft,
                transformOrigin: '0 0',
                transform: `scale(${1 / maxSafeScale})`,
                width: stageWidth * maxSafeScale,
                height: stageHeight * maxSafeScale,
                zIndex: 4,
                touchAction: 'none',
                userSelect: 'none',
                opacity: isShowingOriginal ? 0 : 1,
                transition: 'opacity 150ms ease-in-out',
                ...getEdgeFadeStyle(128),
              }}
            >
              <Stage
                width={stageWidth * maxSafeScale}
                height={stageHeight * maxSafeScale}
                onMouseDown={handleStart}
                onTouchStart={handleStart}
                onMouseEnter={handleMouseEnter}
                onMouseLeave={handleMouseLeave}
                onMouseMove={handleMove}
                onTouchMove={handleMove}
                onMouseUp={handleUp}
                onTouchEnd={handleUp}
              >
                <Layer listening={!showOriginal}>
                  <Group scaleX={maxSafeScale} scaleY={maxSafeScale}>
                    <Group x={groupOffsetX} y={groupOffsetY}>
                      {(isMasking || isAiEditing) &&
                        activeContainer &&
                        sortedSubMasks.map((subMask: SubMask) => {
                          const activeId = isMasking ? activeMaskId : activeAiSubMaskId;
                          const renderSubMask =
                            subMask.id === activeId && localInitialDrawParams
                              ? { ...subMask, parameters: localInitialDrawParams }
                              : subMask;

                          const isCloneOrHeal = renderSubMask.type === Mask.Clone || renderSubMask.type === Mask.Heal;
                          const isThisSubMaskActive = renderSubMask.id === activeId;
                          const isActivelyDrawingThis = isThisSubMaskActive && isDrawing.current;
                          const isHoveringThisMarker = hoveredMarkerId === renderSubMask.id;

                          let showBrushStrokes = true;
                          if (isCloneOrHeal) {
                            showBrushStrokes =
                              isActivelyDrawingThis ||
                              isHoveringThisMarker ||
                              (isThisSubMaskActive && isMaskControlHovered);
                          }

                          return (
                            <MaskOverlay
                              adjustments={adjustments}
                              imageHeight={effectiveImageDimensions.height}
                              imageWidth={effectiveImageDimensions.width}
                              isSelected={renderSubMask.id === activeId}
                              isToolActive={isToolActive}
                              showBrushStrokes={showBrushStrokes}
                              key={renderSubMask.id}
                              onMaskInteractionEnd={handleMaskInteractionEnd}
                              onMaskInteractionStart={handleMaskInteractionStart}
                              onMaskMouseEnter={() => !isToolActive && setIsMaskHovered(true)}
                              onMaskMouseLeave={() => !isToolActive && setIsMaskHovered(false)}
                              onPreviewUpdate={handlePreviewUpdate}
                              onSelect={() =>
                                isMasking ? onSelectMask(renderSubMask.id) : onSelectAiSubMask(renderSubMask.id)
                              }
                              onUpdate={updateSubMask}
                              scale={imageRenderSize.scale}
                              subMask={renderSubMask}
                              offsetX={groupOffsetX}
                              offsetY={groupOffsetY}
                              stageScale={maxSafeScale}
                              penCursor={renderSubMask.id === activeId ? penCursor : null}
                              penActiveAnchor={
                                renderSubMask.id === activeId ? (penDrag.current?.index ?? -1) : -1
                              }
                              onDuplicateSubMask={duplicateSubMaskOnCanvas}
                            />
                          );
                        })}

                      {previewBox && (
                        <Rect
                          x={Math.min(previewBox.start.x, previewBox.end.x)}
                          y={Math.min(previewBox.start.y, previewBox.end.y)}
                          width={Math.max(0.1, Math.abs(previewBox.end.x - previewBox.start.x))}
                          height={Math.max(0.1, Math.abs(previewBox.end.y - previewBox.start.y))}
                          stroke="#0ea5e9"
                          strokeWidth={2}
                          dash={[4, 4]}
                          listening={false}
                        />
                      )}
                      {isBrushActive &&
                        cursorPreview.visible &&
                        (!isManualCleanupActive ||
                          (activeSubMask?.parameters?.sourceX !== undefined && !isCtrlPressed)) && (
                          <Circle
                            {...(brushCursorPreview.colorStops
                              ? {
                                  fillRadialGradientColorStops: brushCursorPreview.colorStops,
                                  fillRadialGradientEndPoint: { x: 0, y: 0 },
                                  fillRadialGradientEndRadius: brushCursorPreview.radius,
                                  fillRadialGradientStartPoint: { x: 0, y: 0 },
                                  fillRadialGradientStartRadius: 0,
                                }
                              : { fill: brushCursorPreview.fill })}
                            listening={false}
                            perfectDrawEnabled={false}
                            radius={brushCursorPreview.radius}
                            x={cursorPreview.x}
                            y={cursorPreview.y}
                          />
                        )}
                    </Group>
                  </Group>
                </Layer>
              </Stage>
            </div>
          )}
        </div>

        <div
          className="absolute inset-0 w-full h-full flex items-center justify-center transition-opacity duration-200"
          style={{
            opacity: isCropViewVisible ? 1 : 0,
            pointerEvents: isCropViewVisible ? 'auto' : 'none',
          }}
        >
          {cropPreviewUrl && uncroppedImageRenderSize && (
            <div
              style={{
                height: uncroppedImageRenderSize.height,
                position: 'relative',
                width: uncroppedImageRenderSize.width,
              }}
            >
              <ReactCrop
                aspect={adjustments.aspectRatio ?? undefined}
                crop={crop ?? undefined}
                onChange={setCrop}
                onComplete={handleCropComplete}
                ruleOfThirds={false}
                renderSelectionAddon={() => {
                  const { width, height } = getCropDimensions();
                  if (width <= 0 || height <= 0) {
                    return null;
                  }
                  const showDenseGrid = isRotationActive && !isStraightenActive;
                  const currentOverlayMode = isRotationActive || isStraightenActive ? 'none' : overlayMode || 'none';
                  return (
                    <>
                      <CompositionOverlays
                        width={width}
                        height={height}
                        mode={currentOverlayMode}
                        rotation={overlayRotation || 0}
                        denseVisible={showDenseGrid}
                      />
                      {/* BLITZRAW: not while straightening, which is a
                          different way of setting the same angle and puts its
                          own line across the photo. Two ways of turning it at
                          once is one too many. */}
                      {!isStraightenActive && (
                        <RotationHandles
                          rotation={shownRotation}
                          onRotateStart={handleCornerRotateStart}
                          onRotate={handleCornerRotate}
                          onRotateEnd={handleCornerRotateEnd}
                        />
                      )}
                      {/* BLITZRAW: only with a ratio locked, which is exactly
                          when react-image-crop takes its own edge handles
                          away. With a free crop its handles are there and do
                          the simpler thing already. */}
                      {!isStraightenActive && crop && uncroppedImageRenderSize && (
                        <SideHandles
                          aspect={adjustments.aspectRatio}
                          crop={crop as any}
                          imageSize={uncroppedImageRenderSize}
                          onResize={handleSideResize}
                          onResizeEnd={handleSideResizeEnd}
                        />
                      )}
                    </>
                  );
                }}
              >
                <img
                  alt="Crop preview"
                  ref={cropImageRef}
                  src={cropPreviewUrl}
                  style={{
                    display: 'block',
                    width: `${uncroppedImageRenderSize.width}px`,
                    height: `${uncroppedImageRenderSize.height}px`,
                    objectFit: 'contain',
                    transform: cropImageTransforms,
                    imageRendering: isMaxZoom ? 'pixelated' : 'auto',
                  }}
                />
              </ReactCrop>

              {isStraightenActive && (
                <Stage
                  height={uncroppedImageRenderSize.height}
                  onMouseDown={handleStraightenMouseDown}
                  onTouchStart={handleStraightenMouseDown}
                  onMouseLeave={handleStraightenMouseLeave}
                  onMouseMove={handleStraightenMouseMove}
                  onTouchMove={handleStraightenMouseMove}
                  onMouseUp={handleStraightenMouseUp}
                  onTouchEnd={handleStraightenMouseUp}
                  style={{
                    position: 'absolute',
                    top: 0,
                    left: 0,
                    zIndex: 10,
                    cursor: 'crosshair',
                    touchAction: 'none',
                  }}
                  width={uncroppedImageRenderSize.width}
                >
                  <Layer>
                    {straightenLine && (
                      <Line
                        dash={[4, 4]}
                        listening={false}
                        points={[
                          straightenLine.start.x,
                          straightenLine.start.y,
                          straightenLine.end.x,
                          straightenLine.end.y,
                        ]}
                        stroke="#0ea5e9"
                        strokeWidth={2}
                      />
                    )}
                  </Layer>
                </Stage>
              )}
            </div>
          )}
        </div>
      </div>
    );
  },
);

export default ImageCanvas;
