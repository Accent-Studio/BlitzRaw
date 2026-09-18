// ============ BLITZRAW: denoise only the bit I am looking at ============
// SCUNet costs about four seconds a megapixel, so a whole frame is a minute,
// and picking a strength by trying one costs a minute per try. A square of
// 1024 pixels is one megapixel and comes back in about a second.
//
// So: drag a box over the part that worries you, and see that part cleaned at
// full size. Nothing is written until the button below is pressed.
//
// Full resolution on purpose. Judging a denoiser on a shrunk preview judges it
// on noise the shrinking already took out.
import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { invoke } from '@tauri-apps/api/core';
import { Loader2 } from 'lucide-react';
import Text from '../ui/Text';
import { TextColors, TextVariants, TextWeights } from '../../types/typography';
import { DenoiseMethod, Invokes } from '../ui/AppProperties';

interface DenoisePatch {
  original: string;
  denoised: string;
  width: number;
  height: number;
  x: number;
  y: number;
  fullWidth: number;
  fullHeight: number;
  tookMs: number;
}

interface DenoisePatchPreviewProps {
  /** The photo being worked on. */
  path: string;
  /** A picture of the whole frame, to drag the box over. */
  navigatorUrl: string | null;
  method: DenoiseMethod;
  /** 0 to 1, as the backend wants it. */
  intensity: number;
  patchSize: number;
}

export default function DenoisePatchPreview({
  path,
  navigatorUrl,
  method,
  intensity,
  patchSize,
}: DenoisePatchPreviewProps) {
  const { t } = useTranslation();
  const [centre, setCentre] = useState({ x: 0.5, y: 0.5 });
  const [patch, setPatch] = useState<DenoisePatch | null>(null);
  const [isRunning, setIsRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showOriginal, setShowOriginal] = useState(false);

  const navRef = useRef<HTMLDivElement>(null);
  const isDragging = useRef(false);
  // Rises on every request. Only the newest one is allowed to land, so dragging
  // the box quickly cannot leave an older square on screen than the one asked
  // for last.
  const requestId = useRef(0);

  const run = useCallback(
    async (cx: number, cy: number) => {
      if (!path) return;
      const mine = ++requestId.current;
      setIsRunning(true);
      setError(null);
      try {
        const result = await invoke<DenoisePatch>(Invokes.DenoisePreviewPatch, {
          path,
          centreX: cx,
          centreY: cy,
          size: patchSize,
          intensity,
          method,
        });
        if (mine !== requestId.current) return;
        setPatch(result);
      } catch (e: any) {
        if (mine !== requestId.current) return;
        setError(typeof e === 'string' ? e : String(e));
      } finally {
        if (mine === requestId.current) setIsRunning(false);
      }
    },
    [path, patchSize, intensity, method],
  );

  // A settle before running, so dragging the box across the frame does not
  // start a job per pixel of travel. The model cannot be cancelled once it has
  // started, so the cheapest cancel is not to start.
  useEffect(() => {
    const timer = setTimeout(() => run(centre.x, centre.y), 250);
    return () => clearTimeout(timer);
  }, [centre, run]);

  const moveTo = useCallback((e: { clientX: number; clientY: number }) => {
    const rect = navRef.current?.getBoundingClientRect();
    if (!rect || rect.width === 0 || rect.height === 0) return;
    setCentre({
      x: Math.min(1, Math.max(0, (e.clientX - rect.left) / rect.width)),
      y: Math.min(1, Math.max(0, (e.clientY - rect.top) / rect.height)),
    });
  }, []);

  useEffect(() => {
    const onMove = (e: MouseEvent) => {
      if (isDragging.current) moveTo(e);
    };
    const onUp = () => {
      isDragging.current = false;
    };
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
    return () => {
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
    };
  }, [moveTo]);

  // Where to draw the box on the navigator. Taken from where the square really
  // landed when there is one, because near an edge it is pushed inside.
  const boxStyle = (() => {
    if (!patch || !patch.fullWidth || !patch.fullHeight) {
      const frac = 0.2;
      return {
        left: `${Math.max(0, Math.min(1 - frac, centre.x - frac / 2)) * 100}%`,
        top: `${Math.max(0, Math.min(1 - frac, centre.y - frac / 2)) * 100}%`,
        width: `${frac * 100}%`,
        height: `${frac * 100}%`,
      };
    }
    return {
      left: `${(patch.x / patch.fullWidth) * 100}%`,
      top: `${(patch.y / patch.fullHeight) * 100}%`,
      width: `${(patch.width / patch.fullWidth) * 100}%`,
      height: `${(patch.height / patch.fullHeight) * 100}%`,
    };
  })();

  const shown = patch ? (showOriginal ? patch.original : patch.denoised) : null;

  return (
    <div className="flex gap-4 w-full h-full min-h-0">
      <div className="flex flex-col gap-2 w-[240px] shrink-0">
        <Text variant={TextVariants.small} weight={TextWeights.medium}>
          {t('modals.denoise.patchNavigator')}
        </Text>
        <div
          ref={navRef}
          className="relative w-full rounded-md overflow-hidden bg-bg-primary cursor-crosshair select-none"
          onMouseDown={(e: any) => {
            isDragging.current = true;
            moveTo(e);
          }}
        >
          {navigatorUrl ? (
            <img src={navigatorUrl} alt="" className="w-full h-auto block pointer-events-none" draggable={false} />
          ) : (
            <div className="w-full aspect-[3/2]" />
          )}
          <div
            className="absolute border-2 border-accent pointer-events-none"
            style={{ ...boxStyle, boxShadow: '0 0 0 9999px rgba(0,0,0,0.45)' }}
          />
        </div>
        <Text variant={TextVariants.small} color={TextColors.secondary}>
          {t('modals.denoise.patchHint')}
        </Text>
        {patch && (
          <Text variant={TextVariants.small} color={TextColors.secondary}>
            {t('modals.denoise.patchTiming', {
              size: patch.width,
              seconds: (patch.tookMs / 1000).toFixed(1),
            })}
          </Text>
        )}
      </div>

      <div className="flex-1 min-w-0 flex flex-col gap-2">
        <div className="flex items-center justify-between">
          <Text variant={TextVariants.small} weight={TextWeights.medium}>
            {showOriginal ? t('modals.denoise.original') : t('modals.denoise.denoised')}
          </Text>
          <button
            type="button"
            className="px-3 py-1 rounded-md bg-bg-primary hover:bg-card-active transition-colors text-sm text-text-secondary"
            // Held down rather than toggled. Flicking back and forth on one
            // spot is how a difference this small is actually seen.
            onMouseDown={() => setShowOriginal(true)}
            onMouseUp={() => setShowOriginal(false)}
            onMouseLeave={() => setShowOriginal(false)}
          >
            {t('modals.denoise.holdForOriginal')}
          </button>
        </div>
        <div className="relative flex-1 min-h-0 rounded-md overflow-hidden bg-bg-primary flex items-center justify-center">
          {shown ? (
            // Nearest neighbour, so what is on screen is the pixels rather than
            // the browser's idea of them.
            <img
              src={shown}
              alt=""
              className="max-w-full max-h-full object-contain"
              style={{ imageRendering: 'pixelated' }}
              draggable={false}
            />
          ) : (
            !error && <Loader2 className="animate-spin text-text-secondary" size={28} />
          )}
          {isRunning && shown && (
            <div className="absolute top-2 right-2 bg-black/60 rounded-full p-1.5">
              <Loader2 className="animate-spin text-white" size={16} />
            </div>
          )}
          {error && (
            <Text variant={TextVariants.small} color={TextColors.secondary} className="px-6 text-center">
              {error}
            </Text>
          )}
        </div>
      </div>
    </div>
  );
}
// ========== BLITZRAW END: denoise only the bit I am looking at ==========
