import { useState, useEffect, useRef, useCallback, useMemo, useLayoutEffect, useImperativeHandle } from 'react';
import { Crop, PercentCrop } from 'react-image-crop';
import { Loader2 } from 'lucide-react';
import clsx from 'clsx';
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'react-toastify';
import { debouncedSetHistory } from '../../hooks/useEditorActions';

import { ImageDimensions, RenderSize, useImageRenderSize } from '../../hooks/useImageRenderSize';
import { Adjustments, AiPatch, MaskContainer } from '../../utils/adjustments';
import {
  calculateCenteredCrop,
  fitCropWithinRotation,
  pixelCropFromPercent,
  getOrientedDimensions,
  rotateCropCenter,
} from '../../utils/cropUtils';
import EditorToolbar from './editor/EditorToolbar';
import ImageCanvas from './editor/ImageCanvas';
import { Mask, SubMask } from './right/Masks';
import { Panel, TransformState, Invokes } from '../ui/AppProperties';
import Text from '../ui/Text';
import { TextColors, TextVariants, TextWeights } from '../../types/typography';
import { useEditorStore } from '../../store/useEditorStore';
import { maskHasAdjustments, shouldShowMaskOverlay } from '../../utils/maskOverlay';
import { useSettingsStore } from '../../store/useSettingsStore';
import { isPanelShowing, useUIStore } from '../../store/useUIStore';
import { useLibraryStore } from '../../store/useLibraryStore';
import { useAiMasking } from '../../hooks/useAiMasking';
import { CropParams, reconcileCropParams } from '../../utils/cropReconcile';
import { useHistoryJump } from '../../hooks/useHistoryJump';

const parseRgb = (rgbStr: string): [number, number, number, number] => {
  const match = rgbStr.match(/[\d.]+/g);
  if (match && match.length >= 3) {
    return [parseFloat(match[0]) / 255, parseFloat(match[1]) / 255, parseFloat(match[2]) / 255, 1.0];
  }
  return [0, 0, 0, 1.0];
};
const checkCropValid = (pixelCrop: Partial<Crop>, imageW: number, imageH: number, rotation: number) => {
  if (pixelCrop.x === undefined || pixelCrop.y === undefined || !pixelCrop.width || !pixelCrop.height) {
    return false;
  }

  const cx = imageW / 2;
  const cy = imageH / 2;
  const rad = (-rotation * Math.PI) / 180;
  const cos = Math.cos(rad);
  const sin = Math.sin(rad);

  const pts = [
    { x: pixelCrop.x, y: pixelCrop.y },
    { x: pixelCrop.x + pixelCrop.width, y: pixelCrop.y },
    { x: pixelCrop.x, y: pixelCrop.y + pixelCrop.height },
    { x: pixelCrop.x + pixelCrop.width, y: pixelCrop.y + pixelCrop.height },
  ];

  for (let i = 0; i < 4; i++) {
    const p = pts[i];
    const nx = cos * (p.x - cx) - sin * (p.y - cy) + cx;
    const ny = sin * (p.x - cx) + cos * (p.y - cy) + cy;
    if (nx < -1 || nx > imageW + 1 || ny < -1 || ny > imageH + 1) {
      return false;
    }
  }
  return true;
};

interface WgpuRenderState {
  useWgpuRenderer: boolean | undefined;
  isReady: boolean;
  hasRenderedFirstFrame: boolean;
  isCropping: boolean;
  uncroppedAdjustedPreviewUrl: string | null;
  showOriginal: boolean;
  bgPrimary: [number, number, number, number];
  bgSecondary: [number, number, number, number];
}

interface EditorProps {
  onBackToLibrary(): void;
  onContextMenu(event: any): void;
  onImageSelect?(path: string, event?: any): void;
  transformWrapperRef: any;
}

export default function Editor({ onBackToLibrary, onContextMenu, onImageSelect, transformWrapperRef }: EditorProps) {
  const appSettings = useSettingsStore((s) => s.appSettings);
  const osPlatform = useSettingsStore((s) => s.osPlatform);
  const isFullScreen = useUIStore((s) => s.isFullScreen);
  const isInstantTransition = useUIStore((s) => s.isInstantTransition);
  const setUI = useUIStore((s) => s.setUI);
  const isLoading = useLibraryStore((s) => s.isViewLoading);
  const selectedImage = useEditorStore((s) => s.selectedImage);
  const adjustments = useEditorStore((s) => s.adjustments);
  const adjustmentsHistory = useEditorStore((s) => s.history);
  const adjustmentsHistoryLabels = useEditorStore((s) => s.historyLabels);
  const adjustmentsHistoryIndex = useEditorStore((s) => s.historyIndex);
  const finalPreviewUrl = useEditorStore((s) => s.finalPreviewUrl);
  const uncroppedAdjustedPreviewUrl = useEditorStore((s) => s.uncroppedAdjustedPreviewUrl);
  const transformedOriginalUrl = useEditorStore((s) => s.transformedOriginalUrl);
  const interactivePatch = useEditorStore((s) => s.interactivePatch);
  const showOriginal = useEditorStore((s) => s.showOriginal);
  const isSliderDragging = useEditorStore((s) => s.isSliderDragging);
  const targetZoom = useEditorStore((s) => s.zoom);
  const originalSize = useEditorStore((s) => s.originalSize);
  const isRotationActive = useEditorStore((s) => s.isRotationActive);
  const overlayMode = useEditorStore((s) => s.overlayMode);
  const overlayRotation = useEditorStore((s) => s.overlayRotation);
  const isStraightenActive = useEditorStore((s) => s.isStraightenActive);
  const isWbPickerActive = useEditorStore((s) => s.isWbPickerActive);
  const liveRotation = useEditorStore((s) => s.liveRotation);
  const cachedPreviewUrl = useEditorStore((s) => s.cachedPreviewUrl);
  const brushSettings = useEditorStore((s) => s.brushSettings);
  const activeMaskContainerId = useEditorStore((s) => s.activeMaskContainerId);
  const activeMaskId = useEditorStore((s) => s.activeMaskId);
  const activeAiPatchContainerId = useEditorStore((s) => s.activeAiPatchContainerId);
  const activeAiSubMaskId = useEditorStore((s) => s.activeAiSubMaskId);
  const isMaskControlHovered = useEditorStore((s) => s.isMaskControlHovered);
  const hoveredMaskContainerId = useEditorStore((s) => s.hoveredMaskContainerId);
  const hasRenderedFirstFrame = useEditorStore((s) => s.hasRenderedFirstFrame);

  const setEditor = useEditorStore((s) => s.setEditor);
  const undo = useEditorStore((s) => s.undo);
  const redo = useEditorStore((s) => s.redo);
  // BLITZRAW: the same jump the History panel uses, so the toolbar's step list
  // and the panel mean the same thing. A click is a thing I did, so it goes into
  // the list of what I did and Ctrl+Z takes the jump back.
  const goToHistoryIndex = useHistoryJump();
  const canUndo = adjustmentsHistoryIndex > 0;
  const canRedo = adjustmentsHistoryIndex < adjustmentsHistory.length - 1;

  const isAndroid = osPlatform === 'android';

  const setAdjustments = useCallback(
    (value: Partial<Adjustments> | ((prev: Adjustments) => Adjustments)) => {
      setEditor((state) => {
        const prevAdjustments = state.adjustments;
        const newAdjustments = typeof value === 'function' ? value(prevAdjustments) : { ...prevAdjustments, ...value };
        // The shared rate limiter, not one of this component's own. Two timers
        // on one value is how a run of nudges ends up split across two steps.
        debouncedSetHistory(newAdjustments, state.selectedImage?.path ?? null);
        return { adjustments: newAdjustments };
      });
    },
    [setEditor],
  );

  const { handleGenerateAiMask, handleQuickErase, handleManualCleanup } = useAiMasking();

  const [crop, setCrop] = useState<Crop | null>(null);
  // BLITZRAW: keyed to a photo. It used to be per mount and start empty, so the
  // first run after mounting reported that everything had changed, including the
  // frame's orientation, and a framing set by hand was replaced with the centred
  // rectangle. See utils/cropReconcile.ts.
  const prevCropParams = useRef<CropParams | null>(null);
  // The rotation a crop was last fitted at with the crop panel shut, per image.
  const looseRotationRef = useRef<{ path: string; rotation: number } | null>(null);
  const lastValidCropRef = useRef<PercentCrop | null>(null);

  const [isMaskHovered, setIsMaskHovered] = useState(false);
  const [isMaskTouchInteracting, setIsMaskTouchInteracting] = useState(false);
  const [isLoaderVisible, setIsLoaderVisible] = useState(false);
  const [showExifDateView, setShowExifDateView] = useState(false);
  const [maskOverlayUrl, setMaskOverlayUrl] = useState<string | null>(null);
  const [transformState, setTransformState] = useState<TransformState>({ scale: 1, positionX: 0, positionY: 0 });

  const imageContainerRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const isInitialMount = useRef(true);
  const transformStateRef = useRef<TransformState>(transformState);
  transformStateRef.current = transformState;
  const [isPanningState, setIsPanningState] = useState(false);
  const isClickAnimating = useRef(false);
  const clickAnimationTime = 250;
  const zoomDebounceTimeoutRef = useRef<number | null>(null);
  const mouseDownPos = useRef<{ x: number; y: number } | null>(null);
  const savedZoomState = useRef<{ scale: number; positionX: number; positionY: number } | null>(null);
  const focalPointRef = useRef({ x: 0.5, y: 0.5 });
  const isTransitioningRef = useRef(false);
  const [toolbarOverflowVisible, setToolbarOverflowVisible] = useState(!isFullScreen);
  const isGeneratingOverlayRef = useRef(false);
  const pendingOverlayRequestRef = useRef<any>(null);
  const animationFrameId = useRef<number | null>(null);
  const physicsFrameId = useRef<number | null>(null);
  const activePointers = useRef<Map<number, { x: number; y: number }>>(new Map());
  const lastPanPos = useRef<{ x: number; y: number } | null>(null);
  const lastPinch = useRef<{ dist: number; midX: number; midY: number } | null>(null);
  const panVelocityHistory = useRef<{ x: number; y: number; t: number }[]>([]);
  const wheelSnapTimeout = useRef<number | null>(null);
  const isMiddleMousePanning = useRef(false);
  const wasPanningDisabledOnDown = useRef(false);

  const prevRenderState = useRef({
    containerLeft: 0,
    containerTop: 0,
    offsetX: 0,
    offsetY: 0,
    width: 0,
  });
  const transitionAnchorRef = useRef<{
    active: boolean;
    screenImageLeft: number;
    screenImageTop: number;
    physicalImageWidth: number;
  } | null>(null);
  const wgpuSyncRef = useRef<number | null>(null);
  const lastWgpuTransformRef = useRef<string | null>(null);

  const toggleShowOriginal = useCallback(
    () => setEditor((state) => ({ showOriginal: !state.showOriginal })),
    [setEditor],
  );

  const handleToggleFullScreen = useCallback(() => {
    const currentlyZoomed = targetZoom > 1.01;
    setUI({ isInstantTransition: currentlyZoomed });

    if (isFullScreen) {
      setUI({ isFullScreen: false });
    } else {
      if (selectedImage) setUI({ isFullScreen: true });
    }

    if (currentlyZoomed) {
      setTimeout(() => setUI({ isInstantTransition: false }), 100);
    }
  }, [isFullScreen, selectedImage, targetZoom, setUI]);

  const handleDisplaySizeChange = useCallback(
    (size: RenderSize) => {
      setEditor({ displaySize: { width: size.width, height: size.height } });
      if (size.scale) {
        const baseWidth = size.width / size.scale;
        const baseHeight = size.height / size.scale;
        const newSize = {
          width: baseWidth,
          height: baseHeight,
          offsetX: size.offsetX || 0,
          offsetY: size.offsetY || 0,
          containerWidth: size.containerWidth || 0,
          containerHeight: size.containerHeight || 0,
        };
        setEditor({ baseRenderSize: newSize });
      }
    },
    [setEditor],
  );

  const handleZoomed = useCallback(
    (state: TransformState) => {
      setEditor({ zoom: state.scale });
    },
    [setEditor],
  );

  const handleStraighten = useCallback(
    (angleCorrection: number) => {
      setAdjustments((prev: Adjustments) => {
        const newRotation = (prev.rotation || 0) + angleCorrection;
        return { ...prev, rotation: newRotation };
      });
      setEditor({ isStraightenActive: false });
    },
    [setAdjustments, setEditor],
  );

  const updateSubMaskLocal = useCallback(
    (subMaskId: string, updatedData: any) => {
      setAdjustments((prev: Adjustments) => ({
        ...prev,
        masks: prev.masks.map((c: MaskContainer) => ({
          ...c,
          subMasks: c.subMasks.map((sm: SubMask) => (sm.id === subMaskId ? { ...sm, ...updatedData } : sm)),
        })),
        aiPatches: (prev.aiPatches || []).map((p: AiPatch) => ({
          ...p,
          subMasks: p.subMasks.map((sm: SubMask) => (sm.id === subMaskId ? { ...sm, ...updatedData } : sm)),
        })),
      }));
    },
    [setAdjustments],
  );

  // BLITZRAW: a pick deliberately does not put the eyedropper down.
  //
  // Finding a neutral takes several tries on different parts of the picture,
  // and a one-shot tool would have to be picked up again between every one of
  // them. What ends it is leaving the photo, which is where somebody has
  // actually finished. See utils/wbPicker.ts and ImageCanvas's handleMove.
  const handleWbPicked = useCallback(() => {}, []);

  useEffect(() => {
    if (isFullScreen) {
      setToolbarOverflowVisible(false);
    } else {
      const timer = setTimeout(() => {
        setToolbarOverflowVisible(true);
      }, 300);
      return () => clearTimeout(timer);
    }
  }, [isFullScreen]);

  // BLITZRAW: a tool is in use when its own panel is the one its sidebar is
  // showing, not when it happens to be the last panel activated anywhere. The
  // two sidebars change independently and sometimes in the same keystroke; see
  // `isPanelShowing` in useUIStore for the keystroke that found this.
  const isCropping = useUIStore((s) => isPanelShowing(s.activePanels, Panel.Crop));
  const isMasking = useUIStore((s) => isPanelShowing(s.activePanels, Panel.Masks));
  const isAiEditing = useUIStore((s) => isPanelShowing(s.activePanels, Panel.Ai));

  const croppedDimensions = useMemo<ImageDimensions | null>(() => {
    if (!selectedImage?.width || !selectedImage?.height) {
      return null;
    }
    if (adjustments.crop) {
      return { width: adjustments.crop.width, height: adjustments.crop.height } as ImageDimensions;
    }
    if (selectedImage) {
      const orientationSteps = adjustments.orientationSteps || 0;
      const isSwapped = orientationSteps === 1 || orientationSteps === 3;
      const width = isSwapped ? selectedImage.height : selectedImage.width;
      const height = isSwapped ? selectedImage.width : selectedImage.height;
      return { width, height } as ImageDimensions;
    }
    return null;
  }, [selectedImage, adjustments.crop, adjustments.orientationSteps]);

  const imageRenderSize = useImageRenderSize(imageContainerRef, croppedDimensions);
  const imageRenderSizeRef = useRef(imageRenderSize);
  imageRenderSizeRef.current = imageRenderSize;

  const transformConfig = useMemo(() => {
    if (!selectedImage || !imageRenderSize.scale || !originalSize) {
      return { minScale: 0.1, maxScale: 20 };
    }

    const dpr = typeof window !== 'undefined' ? window.devicePixelRatio || 1 : 1;
    const scaleFor100Percent = 1 / imageRenderSize.scale;

    const minScale = (0.1 / dpr) * scaleFor100Percent;
    const maxScale = (2.0 / dpr) * scaleFor100Percent;

    return {
      minScale: Math.max(0.1, minScale),
      maxScale: Math.max(20, maxScale),
    };
  }, [selectedImage, imageRenderSize.scale, originalSize]);

  const minScaleRef = useRef(transformConfig.minScale);
  const maxScaleRef = useRef(transformConfig.maxScale);

  useEffect(() => {
    minScaleRef.current = transformConfig.minScale;
    maxScaleRef.current = transformConfig.maxScale;
  }, [transformConfig.minScale, transformConfig.maxScale]);

  const getTransformBounds = useCallback((scale: number) => {
    const container = imageContainerRef.current;
    if (!container) return { minX: 0, maxX: 0, minY: 0, maxY: 0 };

    const cw = container.clientWidth;
    const ch = container.clientHeight;
    const scaledW = cw * scale;
    const scaledH = ch * scale;

    let minX, maxX, minY, maxY;

    if (scaledW <= cw) {
      minX = maxX = (cw - scaledW) / 2;
    } else {
      minX = cw - scaledW;
      maxX = 0;
    }

    if (scaledH <= ch) {
      minY = maxY = (ch - scaledH) / 2;
    } else {
      minY = ch - scaledH;
      maxY = 0;
    }

    return { minX, maxX, minY, maxY };
  }, []);

  const clampToBounds = useCallback(
    (x: number, y: number, scale: number) => {
      const safeScale = Math.min(
        Math.max(Number.isFinite(scale) ? scale : 1, minScaleRef.current),
        maxScaleRef.current,
      );

      const bounds = getTransformBounds(safeScale);

      const safeX = Number.isFinite(x) ? x : 0;
      const safeY = Number.isFinite(y) ? y : 0;

      const newX = Math.min(Math.max(safeX, bounds.minX), bounds.maxX);
      const newY = Math.min(Math.max(safeY, bounds.minY), bounds.maxY);

      return { x: newX, y: newY, scale: safeScale };
    },
    [getTransformBounds],
  );

  const applyTransform = useCallback(
    (x: number, y: number, scale: number) => {
      transformStateRef.current = { positionX: x, positionY: y, scale };
      setTransformState({ scale, positionX: x, positionY: y });

      if (contentRef.current) {
        contentRef.current.style.transform = `translate(${x}px, ${y}px) scale(${scale})`;
      }

      if (!isTransitioningRef.current) {
        if (scale > 1.01) {
          const container = imageContainerRef.current;
          if (container) {
            const cw = container.offsetWidth;
            const ch = container.offsetHeight;
            focalPointRef.current = {
              x: (cw / 2 - x) / (cw * scale),
              y: (ch / 2 - y) / (ch * scale),
            };
          }
        } else {
          focalPointRef.current = { x: 0.5, y: 0.5 };
        }
      }

      if (zoomDebounceTimeoutRef.current) clearTimeout(zoomDebounceTimeoutRef.current);
      zoomDebounceTimeoutRef.current = window.setTimeout(() => {
        handleZoomed({ scale, positionX: x, positionY: y });
      }, 100);
    },
    [handleZoomed],
  );

  const animateTransform = useCallback(
    (targetX: number, targetY: number, targetScale: number, duration: number) => {
      if (physicsFrameId.current) cancelAnimationFrame(physicsFrameId.current);

      const startX = transformStateRef.current.positionX;
      const startY = transformStateRef.current.positionY;
      const startScale = transformStateRef.current.scale;
      const boundedTarget = clampToBounds(targetX, targetY, targetScale);

      const startTime = performance.now();

      const step = (currentTime: number) => {
        const elapsed = currentTime - startTime;
        const progress = Math.min(elapsed / duration, 1);
        const easeProgress = 1 - Math.pow(1 - progress, 3);

        const currX = startX + (boundedTarget.x - startX) * easeProgress;
        const currY = startY + (boundedTarget.y - startY) * easeProgress;
        const currScale = startScale + (boundedTarget.scale - startScale) * easeProgress;

        applyTransform(currX, currY, currScale);

        if (progress < 1) {
          animationFrameId.current = requestAnimationFrame(step);
        }
      };

      if (animationFrameId.current) cancelAnimationFrame(animationFrameId.current);
      animationFrameId.current = requestAnimationFrame(step);
    },
    [applyTransform, clampToBounds],
  );

  const startPhysicsLoop = useCallback(
    (initialVx: number, initialVy: number) => {
      if (physicsFrameId.current) cancelAnimationFrame(physicsFrameId.current);
      if (animationFrameId.current) cancelAnimationFrame(animationFrameId.current);

      let vx = initialVx;
      let vy = initialVy;
      let lastTime = performance.now();

      const step = (time: number) => {
        const dt = Math.min(time - lastTime, 32);
        lastTime = time;

        let { positionX: x, positionY: y, scale } = transformStateRef.current;
        const bounds = getTransformBounds(scale);

        x += vx * dt;
        y += vy * dt;

        const decay = Math.pow(0.994, dt);
        vx *= decay;
        vy *= decay;

        let outOfBounds = false;
        if (x > bounds.maxX || x < bounds.minX || y > bounds.maxY || y < bounds.minY) {
          outOfBounds = true;
        }

        if (outOfBounds) {
          vx *= 0.5;
          vy *= 0.5;

          const correction = 0.15;
          if (x > bounds.maxX) x += (bounds.maxX - x) * correction;
          else if (x < bounds.minX) x += (bounds.minX - x) * correction;

          if (y > bounds.maxY) y += (bounds.maxY - y) * correction;
          else if (y < bounds.minY) y += (bounds.minY - y) * correction;
        }

        applyTransform(x, y, scale);

        const speed = Math.hypot(vx, vy);

        if (speed < 0.02 && !outOfBounds) {
          const finalPos = clampToBounds(x, y, scale);
          if (Math.abs(x - finalPos.x) > 0.05 || Math.abs(y - finalPos.y) > 0.05) {
            applyTransform(finalPos.x, finalPos.y, scale);
          }
          return;
        }

        if (outOfBounds && speed < 0.05 && Math.abs(vx) < 0.05 && Math.abs(vy) < 0.05) {
          const dist = Math.max(
            x > bounds.maxX ? x - bounds.maxX : x < bounds.minX ? bounds.minX - x : 0,
            y > bounds.maxY ? y - bounds.maxY : y < bounds.minY ? bounds.minY - y : 0,
          );
          if (dist < 0.5) {
            const finalPos = clampToBounds(x, y, scale);
            applyTransform(finalPos.x, finalPos.y, scale);
            return;
          }
        }

        physicsFrameId.current = requestAnimationFrame(step);
      };
      physicsFrameId.current = requestAnimationFrame(step);
    },
    [applyTransform, getTransformBounds, clampToBounds],
  );

  const zoomToCenter = useCallback(
    (newScale: number, duration: number) => {
      const container = imageContainerRef.current;
      if (!container) return;
      const cw = container.clientWidth;
      const ch = container.clientHeight;
      const centerX = cw / 2;
      const centerY = ch / 2;

      const ratio = newScale / transformStateRef.current.scale;
      const newX = centerX - (centerX - transformStateRef.current.positionX) * ratio;
      const newY = centerY - (centerY - transformStateRef.current.positionY) * ratio;

      if (duration > 0) {
        animateTransform(newX, newY, newScale, duration);
      } else {
        const bounded = clampToBounds(newX, newY, newScale);
        applyTransform(bounded.x, bounded.y, bounded.scale);
      }
    },
    [animateTransform, applyTransform, clampToBounds],
  );

  useImperativeHandle(
    transformWrapperRef,
    () => ({
      zoomIn: (factor: number, time?: number) => {
        zoomToCenter(transformStateRef.current.scale * Math.exp(factor), time || 0);
      },
      zoomOut: (factor: number, time?: number) => {
        zoomToCenter(transformStateRef.current.scale * Math.exp(-factor), time || 0);
      },
      resetTransform: (time?: number) => {
        if (time) animateTransform(0, 0, 1, time);
        else applyTransform(0, 0, 1);
      },
      setTransform: (x: number, y: number, scale: number, time?: number) => {
        if (time && time > 0) animateTransform(x, y, scale, time);
        else {
          const bounded = clampToBounds(x, y, scale);
          applyTransform(bounded.x, bounded.y, bounded.scale);
        }
      },
      instance: {
        wrapperComponent: imageContainerRef.current,
        contentComponent: contentRef.current,
        get transformState() {
          return transformStateRef.current;
        },
      },
    }),
    [animateTransform, applyTransform, clampToBounds, zoomToCenter],
  );

  useEffect(() => {
    if (!transformWrapperRef.current || !targetZoom || targetZoom <= 0) return;

    const currentScale = transformStateRef.current.scale || 1;
    if (Math.abs(currentScale - targetZoom) < 0.001) return;

    const animationTime = 200;
    if (targetZoom > currentScale) {
      transformWrapperRef.current.zoomIn(Math.log(targetZoom / currentScale), animationTime);
    } else {
      transformWrapperRef.current.zoomOut(Math.log(currentScale / targetZoom), animationTime);
    }
  }, [targetZoom, transformWrapperRef]);

  const activeSubMask = useMemo(() => {
    if (isMasking && activeMaskId) {
      const container = adjustments.masks.find((c: MaskContainer) =>
        c.subMasks.some((sm: SubMask) => sm.id === activeMaskId),
      );
      return container?.subMasks.find((sm) => sm.id === activeMaskId);
    }
    if (isAiEditing && activeAiSubMaskId) {
      const container = adjustments.aiPatches.find((c: AiPatch) =>
        c.subMasks.some((sm: SubMask) => sm.id === activeAiSubMaskId),
      );
      return container?.subMasks?.find((sm: SubMask) => sm.id === activeAiSubMaskId);
    }
    return null;
  }, [adjustments.masks, adjustments.aiPatches, activeMaskId, activeAiSubMaskId, isMasking, isAiEditing]);

  // ============ BLITZRAW: the pen needs the photo to hold still ============
  // A click on the picture zooms it, which is right for looking and wrong for
  // any tool whose own gesture is a click. Every such tool is listed below, and
  // the pen is one.
  //
  // For as long as the pen mask is the selected one, not only while its path is
  // being placed. `isMaskHovered` covers a cursor sitting on an anchor, but not
  // the drag that follows it: the pointer leaves the anchor the moment it
  // moves, hover goes off, and the rest of the drag panned the photo instead of
  // carrying the point. Editing a path is the same kind of work as drawing one
  // and wants the same still picture.
  //
  // The wheel and the middle button are untouched, so the frame can still be
  // zoomed and moved around while a path is being worked on.
  const isPenMaskActive = (isMasking || isAiEditing) && activeSubMask?.type === Mask.Pen;
  // ========== BLITZRAW END: the pen needs the photo to hold still ==========

  const isPanningDisabled =
    isPenMaskActive ||
    isMaskHovered ||
    isMaskTouchInteracting ||
    isCropping ||
    (isMasking &&
      (activeSubMask?.type === Mask.Brush ||
        activeSubMask?.type === Mask.Flow ||
        activeSubMask?.type === Mask.AiSubject ||
        activeSubMask?.type === Mask.Color ||
        activeSubMask?.type === Mask.Luminance ||
        activeSubMask?.parameters?.isInitialDraw)) ||
    (isAiEditing &&
      (activeSubMask?.type === Mask.Brush ||
        activeSubMask?.type === Mask.Flow ||
        activeSubMask?.type === Mask.Clone ||
        activeSubMask?.type === Mask.Heal ||
        activeSubMask?.type === Mask.AiSubject ||
        activeSubMask?.type === Mask.QuickEraser ||
        activeSubMask?.type === Mask.Color ||
        activeSubMask?.type === Mask.Luminance ||
        activeSubMask?.parameters?.isInitialDraw)) ||
    isWbPickerActive;

  useEffect(() => {
    const container = imageContainerRef.current;
    if (!container) return;

    const handleNativeWheel = (e: WheelEvent) => {
      e.preventDefault();
      if (animationFrameId.current) cancelAnimationFrame(animationFrameId.current);
      if (physicsFrameId.current) cancelAnimationFrame(physicsFrameId.current);

      const isPinch = e.ctrlKey;

      const isTrackpad = appSettings?.canvasInputMode === 'trackpad';
      let zoomSpeedMult = appSettings?.zoomSpeedMultiplier ?? 1.0;

      if (isTrackpad) {
        zoomSpeedMult *= 5;
      }

      const isZoomIntent = isPinch || (!isTrackpad && !e.shiftKey && !e.altKey);

      if (isZoomIntent) {
        const rect = container.getBoundingClientRect();
        const mouseX = e.clientX - rect.left;
        const mouseY = e.clientY - rect.top;

        const delta = e.deltaY !== 0 ? e.deltaY : e.deltaX;
        const zoomSensitivity = 0.002 * zoomSpeedMult;
        const exponent = delta * zoomSensitivity;

        let newScale = transformStateRef.current.scale * Math.exp(-exponent);
        newScale = Math.max(minScaleRef.current, Math.min(maxScaleRef.current, newScale));

        const ratio = newScale / transformStateRef.current.scale;
        const newX = mouseX - (mouseX - transformStateRef.current.positionX) * ratio;
        const newY = mouseY - (mouseY - transformStateRef.current.positionY) * ratio;

        const bounded = clampToBounds(newX, newY, newScale);
        applyTransform(bounded.x, bounded.y, bounded.scale);
      } else {
        if (transformStateRef.current.scale <= 1.01) return;

        const { positionX: curX, positionY: curY, scale } = transformStateRef.current;
        const bounds = getTransformBounds(scale);

        let dx = e.deltaX;
        let dy = e.deltaY;

        if (!isTrackpad) {
          if (e.shiftKey && e.altKey) {
            dx = e.deltaY !== 0 ? e.deltaY : e.deltaX;
            dy = dx;
          } else if (e.shiftKey) {
            dx = e.deltaY !== 0 ? e.deltaY : e.deltaX;
            dy = 0;
          } else if (e.altKey) {
            dx = 0;
            dy = e.deltaY !== 0 ? e.deltaY : e.deltaX;
          }
        }

        let newX = curX - dx;
        let newY = curY - dy;

        const resistance = 0.5;

        if (newX > bounds.maxX) newX = bounds.maxX + (newX - bounds.maxX) * resistance;
        else if (newX < bounds.minX) newX = bounds.minX + (newX - bounds.minX) * resistance;

        if (newY > bounds.maxY) newY = bounds.maxY + (newY - bounds.maxY) * resistance;
        else if (newY < bounds.minY) newY = bounds.minY + (newY - bounds.minY) * resistance;

        applyTransform(newX, newY, scale);

        if (wheelSnapTimeout.current) clearTimeout(wheelSnapTimeout.current);
        wheelSnapTimeout.current = window.setTimeout(() => {
          startPhysicsLoop(0, 0);
        }, 150);
      }
    };

    container.addEventListener('wheel', handleNativeWheel, { passive: false });
    return () => container.removeEventListener('wheel', handleNativeWheel);
  }, [
    applyTransform,
    clampToBounds,
    getTransformBounds,
    startPhysicsLoop,
    appSettings?.canvasInputMode,
    appSettings?.zoomSpeedMultiplier,
  ]);

  const handlePointerDown = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      wasPanningDisabledOnDown.current = isPanningDisabled;

      if (e.pointerType === 'mouse' && e.button !== 0 && e.button !== 1) return;

      const isMiddleClick = e.pointerType === 'mouse' && e.button === 1;

      if (isPanningDisabled && !isMiddleClick) return;

      if (isMiddleClick) {
        isMiddleMousePanning.current = true;
      }

      if (animationFrameId.current) cancelAnimationFrame(animationFrameId.current);
      if (physicsFrameId.current) cancelAnimationFrame(physicsFrameId.current);

      panVelocityHistory.current = [];
      mouseDownPos.current = { x: e.clientX, y: e.clientY };
      activePointers.current.set(e.pointerId, { x: e.clientX, y: e.clientY });

      if (activePointers.current.size === 1) {
        lastPanPos.current = { x: e.clientX, y: e.clientY };
        setIsPanningState(true);
      } else if (activePointers.current.size === 2) {
        const pts = Array.from(activePointers.current.values());
        lastPinch.current = {
          dist: Math.hypot(pts[0].x - pts[1].x, pts[0].y - pts[1].y),
          midX: (pts[0].x + pts[1].x) / 2,
          midY: (pts[0].y + pts[1].y) / 2,
        };
      }

      if (e.pointerType === 'mouse') e.currentTarget.setPointerCapture(e.pointerId);
    },
    [isPanningDisabled],
  );

  useEffect(() => {
    if (!isPanningDisabled) return;
    if (isMiddleMousePanning.current) return;

    activePointers.current.clear();
    lastPanPos.current = null;
    lastPinch.current = null;
    panVelocityHistory.current = [];
    mouseDownPos.current = null;
    setIsPanningState(false);
  }, [isPanningDisabled]);

  const handlePointerMove = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      if (!activePointers.current.has(e.pointerId)) return;
      activePointers.current.set(e.pointerId, { x: e.clientX, y: e.clientY });

      const canPan = !isPanningDisabled || isMiddleMousePanning.current;

      if (activePointers.current.size === 1 && lastPanPos.current && isPanningState && canPan) {
        panVelocityHistory.current.push({ x: e.clientX, y: e.clientY, t: performance.now() });
        if (panVelocityHistory.current.length > 6) panVelocityHistory.current.shift();

        let dx = e.clientX - lastPanPos.current.x;
        let dy = e.clientY - lastPanPos.current.y;
        lastPanPos.current = { x: e.clientX, y: e.clientY };

        const bounds = getTransformBounds(transformStateRef.current.scale);
        let curX = transformStateRef.current.positionX;
        let curY = transformStateRef.current.positionY;

        if (curX < bounds.minX && dx < 0) dx *= 0.35;
        if (curX > bounds.maxX && dx > 0) dx *= 0.35;
        if (curY < bounds.minY && dy < 0) dy *= 0.35;
        if (curY > bounds.maxY && dy > 0) dy *= 0.35;

        applyTransform(curX + dx, curY + dy, transformStateRef.current.scale);
      } else if (activePointers.current.size === 2 && lastPinch.current) {
        const pts = Array.from(activePointers.current.values());
        const dist = Math.hypot(pts[0].x - pts[1].x, pts[0].y - pts[1].y);
        const midX = (pts[0].x + pts[1].x) / 2;
        const midY = (pts[0].y + pts[1].y) / 2;

        const distDelta = dist / lastPinch.current.dist;
        let newScale = transformStateRef.current.scale * distDelta;
        newScale = Math.max(minScaleRef.current, Math.min(maxScaleRef.current, newScale));

        const rect = imageContainerRef.current?.getBoundingClientRect();
        if (rect) {
          const mouseX = midX - rect.left;
          const mouseY = midY - rect.top;
          const ratio = newScale / transformStateRef.current.scale;

          const panX = midX - lastPinch.current.midX;
          const panY = midY - lastPinch.current.midY;

          let newX = mouseX - (mouseX - transformStateRef.current.positionX) * ratio + panX;
          let newY = mouseY - (mouseY - transformStateRef.current.positionY) * ratio + panY;

          const bounded = clampToBounds(newX, newY, newScale);
          applyTransform(bounded.x, bounded.y, bounded.scale);
        }

        lastPinch.current = { dist, midX, midY };
      }
    },
    [applyTransform, clampToBounds, getTransformBounds, isPanningDisabled, isPanningState],
  );

  const handlePointerUp = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      activePointers.current.delete(e.pointerId);

      if (e.currentTarget.hasPointerCapture(e.pointerId)) {
        e.currentTarget.releasePointerCapture(e.pointerId);
      }

      if (activePointers.current.size === 1) {
        const pts = Array.from(activePointers.current.values());
        lastPanPos.current = { x: pts[0].x, y: pts[0].y };
        lastPinch.current = null;
      } else if (activePointers.current.size === 0) {
        lastPanPos.current = null;
        lastPinch.current = null;
        setIsPanningState(false);
        isMiddleMousePanning.current = false;

        let vx = 0,
          vy = 0;
        const history = panVelocityHistory.current;
        if (history.length > 1) {
          const first = history[0];
          const last = history[history.length - 1];
          const dt = last.t - first.t;
          if (dt > 0 && performance.now() - last.t < 50) {
            vx = (last.x - first.x) / dt;
            vy = (last.y - first.y) / dt;
          }
        }

        const { positionX, positionY, scale } = transformStateRef.current;
        const bounds = getTransformBounds(scale);
        const outOfBounds =
          positionX > bounds.maxX || positionX < bounds.minX || positionY > bounds.maxY || positionY < bounds.minY;

        if (Math.abs(vx) > 0.05 || Math.abs(vy) > 0.05 || outOfBounds) {
          startPhysicsLoop(vx, vy);
        }
      }
    },
    [getTransformBounds, startPhysicsLoop],
  );

  const handleClick = useCallback(
    (e: React.MouseEvent) => {
      if (e.button !== 0) return;
      if (isPanningDisabled || wasPanningDisabledOnDown.current) return;

      if (mouseDownPos.current) {
        const dx = Math.abs(e.clientX - mouseDownPos.current.x);
        const dy = Math.abs(e.clientY - mouseDownPos.current.y);
        if (dx > 5 || dy > 5) return;
      }

      const currentScale = transformStateRef.current.scale;

      if (isClickAnimating.current || currentScale > 1.01) {
        if (!isClickAnimating.current && currentScale > 1.01) {
          savedZoomState.current = {
            scale: currentScale,
            positionX: transformStateRef.current.positionX,
            positionY: transformStateRef.current.positionY,
          };
        }
        animateTransform(0, 0, 1, clickAnimationTime);
        isClickAnimating.current = false;
      } else {
        isClickAnimating.current = true;
        setTimeout(() => {
          isClickAnimating.current = false;
        }, clickAnimationTime + 50);

        const container = imageContainerRef.current;
        if (!container) return;

        const currentPositionX = transformStateRef.current.positionX;
        const currentPositionY = transformStateRef.current.positionY;

        const rect = container.getBoundingClientRect();
        const mouseX = e.clientX - rect.left;
        const mouseY = e.clientY - rect.top;

        let zoomTarget = savedZoomState.current
          ? savedZoomState.current.scale
          : Math.min(currentScale * 2, maxScaleRef.current);
        const ratio = zoomTarget / currentScale;

        const newPositionX = mouseX - (mouseX - currentPositionX) * ratio;
        const newPositionY = mouseY - (mouseY - currentPositionY) * ratio;

        animateTransform(newPositionX, newPositionY, zoomTarget, clickAnimationTime);
      }
    },
    [isCropping, isMasking, isAiEditing, isWbPickerActive, animateTransform],
  );

  useEffect(() => {
    if (isInitialMount.current) {
      isInitialMount.current = false;
      return;
    }
    if (showOriginal) {
      setEditor({ showOriginal: false });
    }
  }, [adjustments, setEditor]);

  useEffect(() => {
    if (!isMasking && !isAiEditing) {
      setIsMaskTouchInteracting(false);
    }
  }, [isMasking, isAiEditing]);

  const hasDisplayableImage = finalPreviewUrl || selectedImage?.thumbnailUrl;
  const showSpinner = isLoading && !hasDisplayableImage;

  useLayoutEffect(() => {
    const container = imageContainerRef.current;
    if (!container || imageRenderSize.width === 0) return;

    const currentRect = container.getBoundingClientRect();
    const scaleOld = transformStateRef.current.scale;
    const posOldX = transformStateRef.current.positionX;
    const posOldY = transformStateRef.current.positionY;

    if (isInstantTransition && !transitionAnchorRef.current && scaleOld > 1.01) {
      transitionAnchorRef.current = {
        active: true,
        screenImageLeft: prevRenderState.current.containerLeft + posOldX + prevRenderState.current.offsetX * scaleOld,
        screenImageTop: prevRenderState.current.containerTop + posOldY + prevRenderState.current.offsetY * scaleOld,
        physicalImageWidth: prevRenderState.current.width * scaleOld,
      };
    }

    if (!isInstantTransition && transitionAnchorRef.current) {
      transitionAnchorRef.current = null;
    }

    if (transitionAnchorRef.current && transitionAnchorRef.current.active) {
      const anchor = transitionAnchorRef.current;

      const scaleNew = anchor.physicalImageWidth / imageRenderSize.width;

      const posNewX = anchor.screenImageLeft - currentRect.left - imageRenderSize.offsetX * scaleNew;
      const posNewY = anchor.screenImageTop - currentRect.top - imageRenderSize.offsetY * scaleNew;

      if (
        Math.abs(scaleNew - scaleOld) > 0.001 ||
        Math.abs(posNewX - posOldX) > 0.5 ||
        Math.abs(posNewY - posOldY) > 0.5
      ) {
        applyTransform(posNewX, posNewY, scaleNew);
      }
    }

    prevRenderState.current = {
      containerLeft: currentRect.left,
      containerTop: currentRect.top,
      offsetX: imageRenderSize.offsetX,
      offsetY: imageRenderSize.offsetY,
      width: imageRenderSize.width,
    };
  }, [isFullScreen, imageRenderSize, isInstantTransition, applyTransform]);

  useEffect(() => {
    const timer = setTimeout(() => {
      if (handleDisplaySizeChange && imageRenderSize.width > 0) {
        const currentDisplaySize = {
          width: imageRenderSize.width * transformState.scale,
          height: imageRenderSize.height * transformState.scale,
          scale: transformState.scale,
          offsetX: imageRenderSize.offsetX,
          offsetY: imageRenderSize.offsetY,
          containerWidth: imageContainerRef.current?.clientWidth || 0,
          containerHeight: imageContainerRef.current?.clientHeight || 0,
        };
        handleDisplaySizeChange(currentDisplaySize);
      }
    }, 100);
    return () => clearTimeout(timer);
  }, [imageRenderSize, transformState.scale, handleDisplaySizeChange]);

  const processOverlayQueue = useCallback(async () => {
    if (isGeneratingOverlayRef.current || !pendingOverlayRequestRef.current) return;

    const { maskDef, renderSize, jsAdjustments } = pendingOverlayRequestRef.current;
    pendingOverlayRequestRef.current = null;

    if (!maskDef || !maskDef.visible || renderSize.width === 0) {
      setMaskOverlayUrl(null);
      return;
    }

    isGeneratingOverlayRef.current = true;
    try {
      const cropOffset = [jsAdjustments.crop?.x || 0, jsAdjustments.crop?.y || 0];

      const { patchesSentToBackend } = useEditorStore.getState();

      const stripSubMasks = (subMasks: any[]) => {
        if (!Array.isArray(subMasks)) return;
        subMasks.forEach((sm) => {
          if (sm.id && sm.parameters && patchesSentToBackend.has(sm.id)) {
            if (sm.parameters.mask_data_base64 !== undefined) sm.parameters.mask_data_base64 = null;
            if (sm.parameters.maskDataBase64 !== undefined) sm.parameters.maskDataBase64 = null;
          }
        });
      };

      const strippedAdjustments = structuredClone(jsAdjustments);
      if (strippedAdjustments.masks) {
        strippedAdjustments.masks.forEach((m: any) => stripSubMasks(m.subMasks));
      }
      if (strippedAdjustments.aiPatches) {
        strippedAdjustments.aiPatches.forEach((p: any) => stripSubMasks(p.subMasks));
      }

      const strippedMaskDef = structuredClone(maskDef);
      stripSubMasks(strippedMaskDef.subMasks);

      const dataUrl: string = await invoke(Invokes.GenerateMaskOverlay, {
        cropOffset,
        height: Math.round(renderSize.height),
        maskDef: strippedMaskDef,
        scale: renderSize.scale,
        width: Math.round(renderSize.width),
        jsAdjustments: strippedAdjustments,
      });

      if (dataUrl) {
        setMaskOverlayUrl(dataUrl);
      } else {
        setMaskOverlayUrl(null);
      }
    } catch (e) {
      console.error('Failed to generate live mask overlay:', e);
      setMaskOverlayUrl(null);
    } finally {
      isGeneratingOverlayRef.current = false;
      if (pendingOverlayRequestRef.current) {
        requestAnimationFrame(processOverlayQueue);
      }
    }
  }, []);

  const requestMaskOverlay = useCallback(
    (maskDef: any, renderSize: any, currentAdjustments: any) => {
      pendingOverlayRequestRef.current = { maskDef, renderSize, jsAdjustments: currentAdjustments };
      processOverlayQueue();
    },
    [processOverlayQueue],
  );

  const handleLiveMaskPreview = useCallback(
    (maskDef: any) => {
      let normalizedDef = maskDef;
      if (maskDef && !maskDef.adjustments) {
        normalizedDef = {
          ...maskDef,
          adjustments: {},
          opacity: 100,
        };
      }
      requestMaskOverlay(normalizedDef, imageRenderSize, adjustments);
    },
    [imageRenderSize, adjustments, requestMaskOverlay],
  );

  const croppedDimensionsRef = useRef(croppedDimensions);
  useEffect(() => {
    croppedDimensionsRef.current = croppedDimensions;
  }, [croppedDimensions]);

  const wgpuStateRef = useRef<WgpuRenderState>({
    useWgpuRenderer: appSettings?.useWgpuRenderer,
    isReady: selectedImage?.isReady ?? false,
    hasRenderedFirstFrame,
    isCropping,
    uncroppedAdjustedPreviewUrl,
    showOriginal,
    bgPrimary: [24 / 255, 24 / 255, 24 / 255, 1.0],
    bgSecondary: [35 / 255, 35 / 255, 35 / 255, 1.0],
  });
  const syncWgpuRef = useRef<() => void>(() => {});

  useEffect(() => {
    const rootStyle = getComputedStyle(document.documentElement);
    const bgPrimaryStr = rootStyle.getPropertyValue('--app-bg-primary') || 'rgb(24, 24, 24)';
    const bgSecondaryStr = rootStyle.getPropertyValue('--app-bg-secondary') || 'rgb(35, 35, 35)';

    wgpuStateRef.current = {
      useWgpuRenderer: appSettings?.useWgpuRenderer,
      isReady: selectedImage?.isReady ?? false,
      hasRenderedFirstFrame,
      isCropping,
      uncroppedAdjustedPreviewUrl,
      showOriginal,
      bgPrimary: parseRgb(bgPrimaryStr),
      bgSecondary: parseRgb(bgSecondaryStr),
    };
  }, [
    appSettings?.useWgpuRenderer,
    selectedImage?.isReady,
    hasRenderedFirstFrame,
    isCropping,
    uncroppedAdjustedPreviewUrl,
    showOriginal,
    appSettings?.theme,
    finalPreviewUrl,
  ]);

  useEffect(() => {
    syncWgpuRef.current();
  }, [
    appSettings?.useWgpuRenderer,
    selectedImage?.isReady,
    hasRenderedFirstFrame,
    isCropping,
    uncroppedAdjustedPreviewUrl,
    showOriginal,
    appSettings?.theme,
    finalPreviewUrl,
    transformState,
    imageRenderSize,
  ]);

  useEffect(() => {
    let isEffectActive = true;
    let isInvoking = false;

    const scheduleSync = () => {
      if (!isEffectActive || wgpuSyncRef.current !== null) return;
      wgpuSyncRef.current = requestAnimationFrame(() => {
        wgpuSyncRef.current = null;
        syncWgpu();
      });
    };

    const syncWgpu = () => {
      if (!isEffectActive) return;

      const state = wgpuStateRef.current;
      const container = imageContainerRef.current;

      if (!container) {
        scheduleSync();
        return;
      }

      const currentRect = container.getBoundingClientRect();
      const dpr = window.devicePixelRatio || 1;
      const windowWidth = Math.max(window.innerWidth * dpr, 1);
      const windowHeight = Math.max(window.innerHeight * dpr, 1);

      const OVERLAP = 2;
      const clipX = (currentRect.left - OVERLAP) * dpr;
      const clipY = (currentRect.top - OVERLAP) * dpr;
      const clipW = Math.max((currentRect.width + OVERLAP * 2) * dpr, 1);
      const clipH = Math.max((currentRect.height + OVERLAP * 2) * dpr, 1);
      const irs = imageRenderSizeRef.current;

      if (
        currentRect.width < 10 ||
        currentRect.height < 10 ||
        state.useWgpuRenderer === false ||
        !state.isReady ||
        !state.hasRenderedFirstFrame ||
        irs.width === 0 ||
        irs.height === 0
      ) {
        const hiddenTransform = `${windowWidth},${windowHeight},-999999,-999999,1,1,${clipX},${clipY},${clipW},${clipH},${state.bgPrimary?.join(',')},${state.bgSecondary?.join(',')}`;

        if (lastWgpuTransformRef.current !== hiddenTransform && !isInvoking) {
          lastWgpuTransformRef.current = hiddenTransform;
          isInvoking = true;
          invoke('update_wgpu_transform', {
            payload: {
              windowWidth,
              windowHeight,
              x: -999999,
              y: -999999,
              width: 1,
              height: 1,
              clipX,
              clipY,
              clipWidth: clipW,
              clipHeight: clipH,
              bgPrimary: state.bgPrimary || [0, 0, 0, 1],
              bgSecondary: state.bgSecondary || [0, 0, 0, 1],
              pixelated: false,
            },
          })
            .catch(() => {})
            .finally(() => {
              isInvoking = false;
              scheduleSync();
            });
        }
        return;
      }

      const scale = transformStateRef.current.scale;
      const posX = transformStateRef.current.positionX;
      const posY = transformStateRef.current.positionY;

      const cw = currentRect.width;
      const ch = currentRect.height;

      const offsetX = irs.width > 0 ? irs.offsetX : 0;
      const offsetY = irs.height > 0 ? irs.offsetY : 0;
      const baseW = irs.width > 0 ? irs.width : cw;
      const baseH = irs.height > 0 ? irs.height : ch;

      let screenX = (currentRect.left + posX + offsetX * scale) * dpr || 0;
      let screenY = (currentRect.top + posY + offsetY * scale) * dpr || 0;
      let screenW = baseW * scale * dpr || 1;
      let screenH = baseH * scale * dpr || 1;

      const isCropViewVisible = state.isCropping && state.uncroppedAdjustedPreviewUrl;

      if (isCropViewVisible) {
        screenX = -999999;
        screenY = -999999;
        screenW = 1;
        screenH = 1;
      } else {
        screenW = Math.max(screenW, 1);
        screenH = Math.max(screenH, 1);
      }

      const currentTransform = `${windowWidth},${windowHeight},${screenX},${screenY},${screenW},${screenH},${clipX},${clipY},${clipW},${clipH},${state.bgPrimary?.join(',')},${state.bgSecondary?.join(',')}`;

      if (lastWgpuTransformRef.current !== currentTransform && !isInvoking) {
        lastWgpuTransformRef.current = currentTransform;
        isInvoking = true;

        const isZoomedIn = scale >= maxScaleRef.current - 0.5;

        invoke('update_wgpu_transform', {
          payload: {
            windowWidth,
            windowHeight,
            x: screenX,
            y: screenY,
            width: screenW,
            height: screenH,
            clipX,
            clipY,
            clipWidth: clipW,
            clipHeight: clipH,
            bgPrimary: state.bgPrimary || [0, 0, 0, 1],
            bgSecondary: state.bgSecondary || [0, 0, 0, 1],
            pixelated: isZoomedIn,
          },
        })
          .catch((err) => console.warn('WGPU Sync Error:', err))
          .finally(() => {
            isInvoking = false;
            scheduleSync();
          });
      }
    };

    syncWgpuRef.current = scheduleSync;
    syncWgpu();

    const container = imageContainerRef.current;
    const resizeObserver = new ResizeObserver(scheduleSync);
    if (container) {
      resizeObserver.observe(container);
    }
    window.addEventListener('resize', scheduleSync);

    return () => {
      isEffectActive = false;
      if (wgpuSyncRef.current !== null) {
        cancelAnimationFrame(wgpuSyncRef.current);
        wgpuSyncRef.current = null;
      }
      resizeObserver.disconnect();
      window.removeEventListener('resize', scheduleSync);
    };
  }, []);

  // ======== BLITZRAW: the overlay is about the mask under the pointer ========
  // Hovering a row in the list is how you ask "where is this one", so the
  // overlay follows the pointer there and falls back to the selected mask. It
  // used to only ever describe the selected one, which made hovering another
  // row show the wrong shape.
  const overlayMaskDef = useMemo(() => {
    if (isMasking) {
      const wanted = hoveredMaskContainerId ?? activeMaskContainerId;
      if (wanted) {
        return adjustments.masks?.find((c: MaskContainer) => c.id === wanted) ?? null;
      }
    } else if (isAiEditing && activeAiPatchContainerId) {
      return adjustments.aiPatches?.find((p: AiPatch) => p.id === activeAiPatchContainerId) ?? null;
    }
    return null;
  }, [isMasking, isAiEditing, hoveredMaskContainerId, activeMaskContainerId, activeAiPatchContainerId, adjustments]);

  /**
   * Whether the red is drawn. The whole rule is in utils/maskOverlay.ts.
   *
   * A mask nobody has adjusted yet is only a shape, so it is always shown. Once
   * it does something, the red is in the way of the thing it does, and comes
   * back only while its own row is under the pointer.
   */
  const isMaskOverlayWanted = useMemo(() => {
    if (!overlayMaskDef) return false;
    return shouldShowMaskOverlay({
      hasAdjustments: maskHasAdjustments((overlayMaskDef as any).adjustments),
      isHoveredInList: hoveredMaskContainerId === (overlayMaskDef as any).id,
    });
  }, [overlayMaskDef, hoveredMaskContainerId]);
  // ====== BLITZRAW END: the overlay is about the mask under the pointer ======

  const overlayTriggerHash = useMemo(() => {
    const activeMaskDef = overlayMaskDef;

    if (!activeMaskDef) return null;

    const geometryKeys = [
      'crop',
      'rotation',
      'flipHorizontal',
      'flipVertical',
      'orientationSteps',
      'transformDistortion',
      'transformVertical',
      'transformHorizontal',
      'transformRotate',
      'transformAspect',
      'transformScale',
      'transformXOffset',
      'transformYOffset',
      'lensDistortionAmount',
      'lensVignetteAmount',
      'lensTcaAmount',
      'lensDistortionParams',
      'lensMaker',
      'lensModel',
      'lensDistortionEnabled',
      'lensTcaEnabled',
      'lensVignetteEnabled',
    ];

    const geometry: any = {};
    geometryKeys.forEach((k) => {
      geometry[k] = (adjustments as any)[k];
    });

    const subMasks = activeMaskDef.subMasks?.map((sm: any) => {
      const { parameters, ...rest } = sm;
      const cleanParams = { ...parameters };
      const maskDataFingerprint = cleanParams.mask_data_base64
        ? `${cleanParams.mask_data_base64.length}-${cleanParams.mask_data_base64.slice(-20)}`
        : null;
      const maskDataCamelFingerprint = cleanParams.maskDataBase64
        ? `${cleanParams.maskDataBase64.length}-${cleanParams.maskDataBase64.slice(-20)}`
        : null;
      delete cleanParams.mask_data_base64;
      delete cleanParams.maskDataBase64;
      return {
        ...rest,
        parameters: cleanParams,
        _maskDataFingerprint: maskDataFingerprint,
        _maskDataCamelFingerprint: maskDataCamelFingerprint,
      };
    });

    return JSON.stringify({
      id: activeMaskDef.id,
      invert: activeMaskDef.invert,
      // BLITZRAW: whether the mask is shown at all.
      //
      // `processOverlayQueue` already clears the overlay for a mask that is not
      // visible, but it was never reached: turning the eye off changed nothing
      // this hash could see, so the effect that asks for a new overlay did not
      // run and the old red stayed on the photo. Hiding a mask is exactly the
      // moment its overlay has to go.
      visible: activeMaskDef.visible,
      ...('opacity' in activeMaskDef ? { opacity: activeMaskDef.opacity } : {}),
      subMasks,
      geometry,
      renderSize: { w: imageRenderSize.width, h: imageRenderSize.height },
    });
  }, [
    isMasking,
    isAiEditing,
    activeMaskContainerId,
    activeAiPatchContainerId,
    adjustments,
    imageRenderSize.width,
    imageRenderSize.height,
  ]);

  useEffect(() => {
    // The same mask the hash above describes, or the two would disagree about
    // which shape is on screen.
    const maskDefForOverlay = overlayMaskDef
      ? isAiEditing && !isMasking
        ? { ...overlayMaskDef, adjustments: {}, opacity: 100 }
        : { ...overlayMaskDef, adjustments: {} }
      : null;

    requestMaskOverlay(maskDefForOverlay, imageRenderSize, adjustments);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [overlayTriggerHash, requestMaskOverlay, isMasking, isAiEditing, overlayMaskDef, imageRenderSize]);

  useEffect(() => {
    let timer: number;
    if (showSpinner) {
      setIsLoaderVisible(true);
    } else {
      timer = setTimeout(() => setIsLoaderVisible(false), 300);
    }
    return () => clearTimeout(timer);
  }, [showSpinner]);

  useEffect(() => {
    if (!isCropping || !selectedImage?.width) {
      return;
    }

    const { aspectRatio, orientationSteps = 0, crop: currentAdjCrop, rotation = 0 } = adjustments;
    const effectiveRotation = liveRotation !== null && liveRotation !== undefined ? liveRotation : rotation;

    // ============ BLITZRAW: measured against THIS photo, not the last one ============
    // A hand-set 16:9 framing became the centred rectangle and then reached two
    // hundred photos, because this comparison read a note kept per mount rather
    // than per photo. The note starts empty, so on the first run after mounting
    // `undefined !== 0` for orientation, and the panel concluded the frame had
    // been turned on its side. That is one of the two branches that recentres a
    // crop outright rather than refitting it.
    //
    // The rule the sibling effect below already states: merely opening a photo
    // cannot write a crop to it. See utils/cropReconcile.ts.
    const reconciled = reconcileCropParams(prevCropParams.current, {
      path: selectedImage.path,
      rotation,
      aspectRatio: aspectRatio ?? null,
      orientationSteps,
    });
    // Remembered on sight, or the next real change would still have nothing of
    // this photo's to compare against and a chosen ratio would not be refitted.
    if (reconciled.firstSight) {
      prevCropParams.current = reconciled.next;
    }
    const geometryChanged = reconciled.geometryChanged;
    // ========== BLITZRAW END: measured against THIS photo, not the last one ==========

    const isDraggingRotation = liveRotation !== null && liveRotation !== undefined;
    const needsRecalc = currentAdjCrop === null || geometryChanged || isDraggingRotation;

    if (needsRecalc) {
      const isSwapped = orientationSteps === 1 || orientationSteps === 3;
      const W = isSwapped ? selectedImage.height : selectedImage.width;
      const H = isSwapped ? selectedImage.width : selectedImage.height;
      const A = aspectRatio || W / H;

      let nextPixelCrop = currentAdjCrop;
      const { aspectChanged, orientationChanged } = reconciled;
      const rotationChanged = reconciled.rotationChanged || isDraggingRotation;

      let isMaximized = false;
      if (currentAdjCrop) {
        const referenceRotation = reconciled.referenceRotation;
        const maxCropForReference = calculateCenteredCrop(
          selectedImage.width,
          selectedImage.height,
          orientationSteps,
          A,
          referenceRotation,
        );

        if (
          maxCropForReference &&
          Math.abs(currentAdjCrop.x - maxCropForReference.x) <= 2 &&
          Math.abs(currentAdjCrop.y - maxCropForReference.y) <= 2 &&
          Math.abs(currentAdjCrop.width - maxCropForReference.width) <= 2 &&
          Math.abs(currentAdjCrop.height - maxCropForReference.height) <= 2
        ) {
          isMaximized = true;
        }
      }

      if (!currentAdjCrop || orientationChanged) {
        nextPixelCrop = calculateCenteredCrop(
          selectedImage.width,
          selectedImage.height,
          orientationSteps,
          A,
          effectiveRotation,
        );
      } else if (aspectChanged) {
        if (!aspectRatio) {
          nextPixelCrop = currentAdjCrop;
        } else {
          const curW = currentAdjCrop.width;
          const curH = currentAdjCrop.height;
          const curCx = currentAdjCrop.x + curW / 2;
          const curCy = currentAdjCrop.y + curH / 2;

          let newW = curW;
          let newH = curW / A;

          if (newH > curH) {
            newH = curH;
            newW = curH * A;
          }

          nextPixelCrop = {
            unit: 'px',
            x: Math.ceil(curCx - newW / 2),
            y: Math.ceil(curCy - newH / 2),
            width: Math.floor(newW),
            height: Math.floor(newH),
          };
        }

        if (!checkCropValid(nextPixelCrop, W, H, effectiveRotation)) {
          nextPixelCrop = calculateCenteredCrop(
            selectedImage.width,
            selectedImage.height,
            orientationSteps,
            A,
            effectiveRotation,
          );
        }
      } else if (isMaximized && rotationChanged) {
        nextPixelCrop = calculateCenteredCrop(
          selectedImage.width,
          selectedImage.height,
          orientationSteps,
          A,
          effectiveRotation,
        );
      } else {
        const referenceRotation = reconciled.referenceRotation;
        const rotationDelta = effectiveRotation - referenceRotation;
        const followedCrop =
          rotationChanged && rotationDelta !== 0
            ? rotateCropCenter(currentAdjCrop, W, H, rotationDelta)
            : currentAdjCrop;

        // Was written out here. Moved to cropUtils unchanged apart from the
        // rounding, so the same fit is reachable when rotation moves without
        // this panel being open.
        nextPixelCrop = fitCropWithinRotation(
          followedCrop,
          selectedImage.width,
          selectedImage.height,
          orientationSteps,
          A,
          effectiveRotation,
        );
      }

      if (isDraggingRotation) {
        if (nextPixelCrop) {
          const pc: PercentCrop = {
            unit: '%',
            x: (nextPixelCrop.x / W) * 100,
            y: (nextPixelCrop.y / H) * 100,
            width: (nextPixelCrop.width / W) * 100,
            height: (nextPixelCrop.height / H) * 100,
          };
          setCrop(pc);
          lastValidCropRef.current = pc;
        }
      } else {
        prevCropParams.current = reconciled.next;

        // ============ BLITZRAW: a box to drag is not a crop to save ============
        // A photo with no crop keeps its whole frame, so the panel offers the
        // whole frame as a box. That used to be written into the photo, which
        // dirtied its settings file and rebuilt its thumbnail for a photo that
        // was only looked at, and worse, counted as an edit: with a selection
        // live it went out to every photo in it. Six photos were cropped that
        // way seconds before two hundred were.
        //
        // Shown, not saved. The drag itself writes it, through the same handler
        // every other crop goes through, so nothing else changes.
        const onlyBecauseThereIsNoCrop = !currentAdjCrop && !geometryChanged;

        if (nextPixelCrop && onlyBecauseThereIsNoCrop) {
          const pc: PercentCrop = {
            unit: '%',
            x: (nextPixelCrop.x / W) * 100,
            y: (nextPixelCrop.y / H) * 100,
            width: (nextPixelCrop.width / W) * 100,
            height: (nextPixelCrop.height / H) * 100,
          };
          setCrop(pc);
          lastValidCropRef.current = pc;
          return;
        }
        // ========== BLITZRAW END: a box to drag is not a crop to save ==========

        if (
          nextPixelCrop &&
          (!currentAdjCrop ||
            Math.abs(currentAdjCrop.x - nextPixelCrop.x) > 1 ||
            Math.abs(currentAdjCrop.y - nextPixelCrop.y) > 1 ||
            Math.abs(currentAdjCrop.width - nextPixelCrop.width) > 1 ||
            Math.abs(currentAdjCrop.height - nextPixelCrop.height) > 1)
        ) {
          setAdjustments((prev: Adjustments) => ({ ...prev, crop: nextPixelCrop }));
        }
      }
    }
  }, [
    adjustments.aspectRatio,
    adjustments.crop,
    adjustments.orientationSteps,
    adjustments.rotation,
    liveRotation,
    isCropping,
    selectedImage,
    setAdjustments,
  ]);

  // The same job for straightening that happens with the crop panel shut.
  //
  // Everything above is behind `isCropping`, and rotation does not need the
  // panel to move: the rotation quick adjustment nudges it from the editor with
  // any panel showing and from the grid, and pasting Transform without Crop
  // sets it too. Rotation fills the corners of a canvas that never changes
  // size, so a crop left where it was reaches straight into them.
  //
  // Deliberately not a second constrain. It decides nothing itself: it applies
  // the two rules the panel applies to a rotation change, and hands the panel's
  // own routine the arithmetic. Nothing happens while the crop is both valid
  // and not at the maximum, so merely opening a photo cannot write a crop to it.
  useEffect(() => {
    if (isCropping || !selectedImage?.path || !selectedImage?.width || !selectedImage?.height) {
      return;
    }
    const { rotation = 0, aspectRatio = null, orientationSteps = 0, crop } = adjustments;
    const { width: W, height: H } = getOrientedDimensions(selectedImage.width, selectedImage.height, orientationSteps);
    const A = aspectRatio || W / H;

    // The rotation this crop was last fitted at. Kept per image, because a
    // reference left over from the previous photo would be answering the
    // maximised question about a crop it knows nothing about.
    const previous =
      looseRotationRef.current?.path === selectedImage.path ? looseRotationRef.current.rotation : rotation;
    looseRotationRef.current = { path: selectedImage.path, rotation };

    // No crop means the whole frame is kept, corners included, so that is what
    // has to be judged rather than treated as nothing to do. It is also the
    // maximum for a level photo, which is what lets the first press out of zero
    // land on the exact maximum rather than a fitted approximation of it.
    const current = crop ?? { unit: 'px' as const, x: 0, y: 0, width: W, height: H };
    const maxForReference = calculateCenteredCrop(
      selectedImage.width,
      selectedImage.height,
      orientationSteps,
      A,
      previous,
    );
    const isMaximized =
      !!maxForReference &&
      Math.abs(current.x - maxForReference.x) <= 2 &&
      Math.abs(current.y - maxForReference.y) <= 2 &&
      Math.abs(current.width - maxForReference.width) <= 2 &&
      Math.abs(current.height - maxForReference.height) <= 2;

    let next: Crop | null = null;
    if (isMaximized && previous !== rotation) {
      // A crop sitting at the maximum follows the rotation in both directions,
      // so straightening back towards level reclaims the frame it gave up.
      // Without this it only ever shrank, and returning to zero left the photo
      // cropped for a rotation it no longer has.
      next = calculateCenteredCrop(selectedImage.width, selectedImage.height, orientationSteps, A, rotation);
    } else if (rotation && !checkCropValid(current, W, H, rotation)) {
      // A crop the user sized by hand is left where it is while it still fits,
      // the same as in the panel, and only shrunk once it does not.
      next = fitCropWithinRotation(
        current,
        selectedImage.width,
        selectedImage.height,
        orientationSteps,
        A,
        rotation,
      );
    }

    // If the answer came back no better, leave it alone rather than writing the
    // same crop back and being called again by the write.
    if (!next || !checkCropValid(next, W, H, rotation)) {
      return;
    }
    if (
      crop &&
      Math.abs(crop.x - next.x) <= 1 &&
      Math.abs(crop.y - next.y) <= 1 &&
      Math.abs(crop.width - next.width) <= 1 &&
      Math.abs(crop.height - next.height) <= 1
    ) {
      return;
    }
    setAdjustments((prev: Adjustments) => ({ ...prev, crop: next }));
  }, [
    isCropping,
    selectedImage,
    adjustments.rotation,
    adjustments.crop,
    adjustments.aspectRatio,
    adjustments.orientationSteps,
    setAdjustments,
  ]);

  useEffect(() => {
    if (!isCropping || !selectedImage?.width) {
      setCrop(null);
      return;
    }

    if (liveRotation !== null && liveRotation !== undefined) {
      return;
    }

    const orientationSteps = adjustments.orientationSteps || 0;
    const isSwapped = orientationSteps === 1 || orientationSteps === 3;
    const cropBaseWidth = isSwapped ? selectedImage.height : selectedImage.width;
    const cropBaseHeight = isSwapped ? selectedImage.width : selectedImage.height;

    const { crop: pixelCrop } = adjustments;

    if (pixelCrop) {
      const pct: PercentCrop = {
        unit: '%',
        x: (pixelCrop.x / cropBaseWidth) * 100,
        y: (pixelCrop.y / cropBaseHeight) * 100,
        width: (pixelCrop.width / cropBaseWidth) * 100,
        height: (pixelCrop.height / cropBaseHeight) * 100,
      };
      setCrop(pct);
      lastValidCropRef.current = pct;
    }
  }, [isCropping, adjustments.crop, adjustments.orientationSteps, selectedImage, liveRotation]);

  const handleCropChange = useCallback(
    (_pixelCrop: Crop, percentCrop: PercentCrop) => {
      if (!selectedImage) return;

      const orientationSteps = adjustments.orientationSteps || 0;
      const isSwapped = orientationSteps === 1 || orientationSteps === 3;
      const W = isSwapped ? selectedImage.height : selectedImage.width;
      const H = isSwapped ? selectedImage.width : selectedImage.height;
      const rotation = liveRotation !== null && liveRotation !== undefined ? liveRotation : adjustments.rotation || 0;

      const MIN_CROP_PX = 64;
      const minPctW = (MIN_CROP_PX / W) * 100;
      const minPctH = (MIN_CROP_PX / H) * 100;

      if (percentCrop.width < minPctW || percentCrop.height < minPctH) {
        return;
      }

      const toPixel = (pc: PercentCrop): Crop => ({
        unit: 'px',
        x: (pc.x / 100) * W,
        y: (pc.y / 100) * H,
        width: (pc.width / 100) * W,
        height: (pc.height / 100) * H,
      });

      if (checkCropValid(toPixel(percentCrop), W, H, rotation)) {
        setCrop(percentCrop);
        lastValidCropRef.current = percentCrop;
        return;
      }

      if (!lastValidCropRef.current) {
        setCrop(percentCrop);
        lastValidCropRef.current = percentCrop;
        return;
      }

      if (!checkCropValid(toPixel(lastValidCropRef.current), W, H, rotation)) {
        const lv = lastValidCropRef.current;
        const cx = lv.x + lv.width / 2;
        const cy = lv.y + lv.height / 2;
        let lo = 0;
        let hi = 1;
        let healed: PercentCrop = lv;
        for (let i = 0; i < 15; i++) {
          const mid = (lo + hi) / 2;
          const factor = 1 - mid;
          const test: PercentCrop = {
            unit: '%',
            x: cx - (lv.width / 2) * factor,
            y: cy - (lv.height / 2) * factor,
            width: lv.width * factor,
            height: lv.height * factor,
          };
          if (checkCropValid(toPixel(test), W, H, rotation)) {
            healed = test;
            hi = mid;
          } else {
            lo = mid;
          }
        }
        lastValidCropRef.current = healed;
      }

      const lastValid = lastValidCropRef.current;
      const oldL = lastValid.x;
      const oldT = lastValid.y;
      const oldR = lastValid.x + lastValid.width;
      const oldB = lastValid.y + lastValid.height;
      const oldW = lastValid.width;
      const oldH = lastValid.height;

      const newL = percentCrop.x;
      const newT = percentCrop.y;
      const newR = percentCrop.x + percentCrop.width;
      const newB = percentCrop.y + percentCrop.height;
      const newW = percentCrop.width;
      const newH = percentCrop.height;

      if (Math.abs(newW - oldW) < 1e-3 && Math.abs(newH - oldH) < 1e-3) {
        let finalCrop = { ...lastValid };

        const applyAxis = (axis: 'X' | 'Y') => {
          let low = 0,
            high = 1;
          let bestValid = { ...finalCrop };

          for (let i = 0; i < 15; i++) {
            const mid = (low + high) / 2;
            const testCrop = { ...finalCrop };

            if (axis === 'X') {
              testCrop.x = finalCrop.x + (percentCrop.x - lastValid.x) * mid;
            } else {
              testCrop.y = finalCrop.y + (percentCrop.y - lastValid.y) * mid;
            }

            if (checkCropValid(toPixel(testCrop), W, H, rotation)) {
              bestValid = { ...testCrop };
              low = mid;
            } else {
              high = mid;
            }
          }
          finalCrop = bestValid;
        };

        const dx = Math.abs(percentCrop.x - lastValid.x);
        const dy = Math.abs(percentCrop.y - lastValid.y);

        if (dx > dy) {
          applyAxis('X');
          applyAxis('Y');
        } else {
          applyAxis('Y');
          applyAxis('X');
        }

        setCrop(finalCrop);
        lastValidCropRef.current = finalCrop;
        return;
      }

      const lastRatio = oldW / oldH;
      const newRatio = newW / newH;
      const isProportional = adjustments.aspectRatio || Math.abs(lastRatio - newRatio) < 0.005;

      if (isProportional) {
        const oldCX = oldL + oldW / 2;
        const oldCY = oldT + oldH / 2;
        const newCX = newL + newW / 2;
        const newCY = newT + newH / 2;

        const dTL = Math.hypot(newL - oldL, newT - oldT);
        const dTR = Math.hypot(newR - oldR, newT - oldT);
        const dBL = Math.hypot(newL - oldL, newB - oldB);
        const dBR = Math.hypot(newR - oldR, newB - oldB);
        const dTC = Math.hypot(newCX - oldCX, newT - oldT);
        const dBC = Math.hypot(newCX - oldCX, newB - oldB);
        const dLC = Math.hypot(newL - oldL, newCY - oldCY);
        const dRC = Math.hypot(newR - oldR, newCY - oldCY);
        const dC = Math.hypot(newCX - oldCX, newCY - oldCY);

        const minD = Math.min(dTL, dTR, dBL, dBR, dTC, dBC, dLC, dRC, dC);

        let targetCrop: PercentCrop = { ...percentCrop };

        if (minD === dTL) {
          targetCrop = { unit: '%', x: oldL, y: oldT, width: newW, height: newH };
        } else if (minD === dTR) {
          targetCrop = { unit: '%', x: oldR - newW, y: oldT, width: newW, height: newH };
        } else if (minD === dBL) {
          targetCrop = { unit: '%', x: oldL, y: oldB - newH, width: newW, height: newH };
        } else if (minD === dBR) {
          targetCrop = { unit: '%', x: oldR - newW, y: oldB - newH, width: newW, height: newH };
        } else if (minD === dTC) {
          targetCrop = { unit: '%', x: oldCX - newW / 2, y: oldT, width: newW, height: newH };
        } else if (minD === dBC) {
          targetCrop = { unit: '%', x: oldCX - newW / 2, y: oldB - newH, width: newW, height: newH };
        } else if (minD === dLC) {
          targetCrop = { unit: '%', x: oldL, y: oldCY - newH / 2, width: newW, height: newH };
        } else if (minD === dRC) {
          targetCrop = { unit: '%', x: oldR - newW, y: oldCY - newH / 2, width: newW, height: newH };
        } else if (minD === dC) {
          targetCrop = { unit: '%', x: oldCX - newW / 2, y: oldCY - newH / 2, width: newW, height: newH };
        }

        const isValidInitially = checkCropValid(toPixel(targetCrop), W, H, rotation);

        if (newW <= oldW && isValidInitially) {
          setCrop(targetCrop);
          lastValidCropRef.current = targetCrop;
        } else {
          let low = 0;
          let high = 1;
          let bestValid = { ...lastValid };

          for (let i = 0; i < 15; i++) {
            const mid = (low + high) / 2;
            const testCrop: PercentCrop = {
              unit: '%',
              x: oldL + (targetCrop.x - oldL) * mid,
              y: oldT + (targetCrop.y - oldT) * mid,
              width: oldW + (targetCrop.width - oldW) * mid,
              height: oldH + (targetCrop.height - oldH) * mid,
            };

            if (checkCropValid(toPixel(testCrop), W, H, rotation)) {
              bestValid = testCrop;
              low = mid;
            } else {
              high = mid;
            }
          }
          setCrop(bestValid);
          lastValidCropRef.current = bestValid;
        }
      } else {
        const eps = 1e-3;
        const tgtL = Math.abs(newL - oldL) < eps ? oldL : newL;
        const tgtT = Math.abs(newT - oldT) < eps ? oldT : newT;
        const tgtR = Math.abs(newR - oldR) < eps ? oldR : newR;
        const tgtB = Math.abs(newB - oldB) < eps ? oldB : newB;

        let currL = tgtL > oldL ? tgtL : oldL;
        let currT = tgtT > oldT ? tgtT : oldT;
        let currR = tgtR < oldR ? tgtR : oldR;
        let currB = tgtB < oldB ? tgtB : oldB;

        const expandEdge = (edge: 'L' | 'T' | 'R' | 'B', target: number) => {
          let low = 0,
            high = 1;
          let startVal = edge === 'L' ? currL : edge === 'T' ? currT : edge === 'R' ? currR : currB;
          let bestVal = startVal;

          for (let i = 0; i < 15; i++) {
            let mid = (low + high) / 2;
            let testVal = startVal + (target - startVal) * mid;

            let testCrop: PercentCrop = {
              unit: '%',
              x: edge === 'L' ? testVal : currL,
              y: edge === 'T' ? testVal : currT,
              width: (edge === 'R' ? testVal : currR) - (edge === 'L' ? testVal : currL),
              height: (edge === 'B' ? testVal : currB) - (edge === 'T' ? testVal : currT),
            };

            if (checkCropValid(toPixel(testCrop), W, H, rotation)) {
              bestVal = testVal;
              low = mid;
            } else {
              high = mid;
            }
          }

          if (edge === 'L') currL = bestVal;
          if (edge === 'T') currT = bestVal;
          if (edge === 'R') currR = bestVal;
          if (edge === 'B') currB = bestVal;
        };

        const expansions: Array<{ edge: 'L' | 'T' | 'R' | 'B'; target: number; delta: number }> = [];
        if (tgtL < oldL) expansions.push({ edge: 'L', target: tgtL, delta: oldL - tgtL });
        if (tgtT < oldT) expansions.push({ edge: 'T', target: tgtT, delta: oldT - tgtT });
        if (tgtR > oldR) expansions.push({ edge: 'R', target: tgtR, delta: tgtR - oldR });
        if (tgtB > oldB) expansions.push({ edge: 'B', target: tgtB, delta: tgtB - oldB });

        expansions.sort((a, b) => b.delta - a.delta);

        for (const exp of expansions) {
          expandEdge(exp.edge, exp.target);
        }

        const finalCrop: PercentCrop = {
          unit: '%',
          x: currL,
          y: currT,
          width: currR - currL,
          height: currB - currT,
        };

        setCrop(finalCrop);
        lastValidCropRef.current = finalCrop;
      }
    },
    [selectedImage, adjustments.orientationSteps, adjustments.rotation, adjustments.aspectRatio, liveRotation],
  );

  const handleCropComplete = useCallback(
    (_: any, pc: PercentCrop) => {
      if (!pc.width || !pc.height || !selectedImage?.width) {
        return;
      }
      if (liveRotation !== null && liveRotation !== undefined) {
        return;
      }

      const orientationSteps = adjustments.orientationSteps || 0;
      const isSwapped = orientationSteps === 1 || orientationSteps === 3;

      const baseW = isSwapped ? selectedImage.height : selectedImage.width;
      const baseH = isSwapped ? selectedImage.width : selectedImage.height;

      // BLITZRAW: the trip back from the box on screen has to land exactly
      // where it started, or opening the Crop panel edits the photo. See
      // utils/cropUtils.ts.
      const newPixelCrop: Crop = pixelCropFromPercent(pc, baseW, baseH);

      setAdjustments((prev: Adjustments) => {
        if (JSON.stringify(newPixelCrop) !== JSON.stringify(prev.crop)) {
          return { ...prev, crop: newPixelCrop };
        }
        return prev;
      });
    },
    [selectedImage, adjustments.orientationSteps, setAdjustments, liveRotation],
  );

  if (!selectedImage) {
    return null;
  }

  const isZoomActionActive = !isPanningDisabled;
  const isMaxZoom = transformState.scale >= maxScaleRef.current - 0.5;

  let cursorStyle = 'default';
  if (isPanningState && isMiddleMousePanning.current) {
    cursorStyle = 'grabbing';
  } else if (isZoomActionActive) {
    if (isPanningState) {
      cursorStyle = 'grabbing';
    } else if (transformState.scale > 1.01) {
      cursorStyle = 'zoom-out';
    } else {
      cursorStyle = 'zoom-in';
    }
  }

  const isWgpuActive = appSettings?.useWgpuRenderer !== false && hasRenderedFirstFrame;
  const hasRenderedAnyPreview = hasRenderedFirstFrame || !!finalPreviewUrl;

  return (
    <div
      className={clsx(
        'flex-1 flex flex-col relative overflow-hidden min-h-0',
        !isInstantTransition && 'transition-all duration-300 ease-in-out',
        isFullScreen
          ? 'rounded-none p-0 gap-0'
          : clsx('rounded-lg p-2 gap-2', appSettings?.useWgpuRenderer !== false ? 'bg-transparent' : 'bg-bg-secondary'),
      )}
    >
      {hasRenderedAnyPreview && <div className="hidden" data-bench-id="editor-first-frame" />}
      <div
        className={clsx(
          'shrink-0 relative z-10',
          !isInstantTransition && 'transition-all duration-300 ease-in-out',
          isFullScreen ? 'max-h-0 opacity-0 m-0' : 'max-h-25 opacity-100',
          toolbarOverflowVisible ? 'overflow-visible' : 'overflow-hidden',
        )}
      >
        <EditorToolbar
          canRedo={canRedo}
          canUndo={canUndo}
          isAndroid={isAndroid}
          isLoading={isLoading}
          onBackToLibrary={onBackToLibrary}
          onImageSelect={onImageSelect}
          onRedo={redo}
          onToggleFullScreen={handleToggleFullScreen}
          onToggleShowOriginal={toggleShowOriginal}
          onUndo={undo}
          selectedImage={selectedImage}
          showOriginal={showOriginal}
          showDateView={showExifDateView}
          onToggleDateView={() => setShowExifDateView((prev) => !prev)}
          adjustmentsHistory={adjustmentsHistory}
          adjustmentsHistoryLabels={adjustmentsHistoryLabels}
          adjustmentsHistoryIndex={adjustmentsHistoryIndex}
          goToAdjustmentsHistoryIndex={goToHistoryIndex}
        />
      </div>

      <div
        className={clsx(
          'flex-1 relative overflow-hidden touch-none',
          isFullScreen ? 'rounded-none' : 'rounded-lg',
          appSettings?.useWgpuRenderer !== false && !isFullScreen && 'ring-[9999px] ring-bg-secondary',
          !isWgpuActive && 'bg-bg-secondary',
        )}
        style={{ cursor: cursorStyle }}
        onContextMenu={onContextMenu}
        ref={imageContainerRef}
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={handlePointerUp}
        onPointerCancel={handlePointerUp}
        onClick={handleClick}
      >
        {showSpinner && (
          <div
            className={clsx(
              'absolute inset-0 bg-bg-secondary/80 flex items-center justify-center z-50 transition-opacity duration-300',
              isLoaderVisible ? 'opacity-100' : 'opacity-0 pointer-events-none',
            )}
          >
            <Loader2 size={48} className="animate-spin text-accent" />
          </div>
        )}

        <div
          ref={contentRef}
          className="w-full h-full flex items-center justify-center origin-top-left"
          style={{
            transform: `translate(${transformState.positionX}px, ${transformState.positionY}px) scale(${transformState.scale})`,
          }}
        >
          <ImageCanvas
            appSettings={appSettings}
            activeAiPatchContainerId={activeAiPatchContainerId}
            activeAiSubMaskId={activeAiSubMaskId}
            activeMaskContainerId={activeMaskContainerId}
            activeMaskId={activeMaskId}
            adjustments={adjustments}
            brushSettings={brushSettings}
            crop={crop}
            finalPreviewUrl={finalPreviewUrl}
            handleCropComplete={handleCropComplete}
            imageRenderSize={imageRenderSize}
            interactivePatch={interactivePatch}
            isAiEditing={isAiEditing}
            isCropping={isCropping}
            isMaskControlHovered={isMaskControlHovered}
            isMaskOverlayWanted={isMaskOverlayWanted}
            isMasking={isMasking}
            isStraightenActive={isStraightenActive}
            isRotationActive={isRotationActive}
            isSliderDragging={isSliderDragging}
            maskOverlayUrl={maskOverlayUrl}
            onGenerateAiMask={handleGenerateAiMask}
            onSelectAiPatchContainer={(id) => setEditor({ activeAiPatchContainerId: id })}
            onSelectMaskContainer={(id) => setEditor({ activeMaskContainerId: id })}
            onLiveMaskPreview={handleLiveMaskPreview}
            onManualCleanup={handleManualCleanup}
            onQuickErase={handleQuickErase}
            onSelectAiSubMask={(id) => setEditor({ activeAiSubMaskId: id })}
            onSelectMask={(id) => setEditor({ activeMaskId: id })}
            onStraighten={handleStraighten}
            selectedImage={selectedImage}
            setCrop={handleCropChange}
            setIsMaskHovered={setIsMaskHovered}
            setIsMaskTouchInteracting={setIsMaskTouchInteracting}
            showOriginal={showOriginal}
            transformedOriginalUrl={transformedOriginalUrl}
            uncroppedAdjustedPreviewUrl={uncroppedAdjustedPreviewUrl}
            updateSubMask={updateSubMaskLocal}
            isWbPickerActive={isWbPickerActive}
            onWbPicked={handleWbPicked}
            setAdjustments={setAdjustments}
            overlayRotation={overlayRotation}
            overlayMode={overlayMode}
            cursorStyle={cursorStyle}
            isMaxZoom={isMaxZoom}
            liveRotation={liveRotation}
            cachedPreviewUrl={cachedPreviewUrl}
            transformState={transformState}
            hasRenderedFirstFrame={hasRenderedFirstFrame}
          />
        </div>
      </div>
    </div>
  );
}
