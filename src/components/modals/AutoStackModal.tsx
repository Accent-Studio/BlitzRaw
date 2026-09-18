import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from 'react-i18next';
import Text from '../ui/Text';
import { TextVariants } from '../../types/typography';

/**
 * Proposing stacks over a selection, for the user to confirm.
 *
 * Two detectors, one dialog, because everything around the controls is the
 * same: the debounce that keeps a slider drag from queueing a request per
 * pixel, the preview panel, the count on the button, and the promise that
 * nothing is stacked until it is pressed.
 *
 * **Brackets** key on the exposure sequence rather than on timing, so their
 * controls are which sizes to look for and how much slack to allow beyond what
 * each frame's own shutter time already justifies.
 *
 * **Bursts** are the opposite and have no exposure signature at all, so timing
 * is the only control there is. Shutter time does not appear: a burst is event
 * work at 1/200 or faster, so there is no in-camera noise reduction to make
 * room for. See `propose_bursts`.
 */

export type StackMode = 'brackets' | 'bursts';

export interface ProposedStack {
  paths: Array<string>;
  exposureValues: Array<number>;
  spanSeconds: number;
}

export interface AutoStackPreview {
  stacks: Array<ProposedStack>;
  sizeCounts: Record<string, number>;
  ungrouped: Array<string>;
}

interface AutoStackModalProps {
  isOpen: boolean;
  targetPaths: Array<string>;
  mode?: StackMode;
  onClose(): void;
  onApply(stacks: Array<ProposedStack>): void;
}

const SELECTABLE_SIZES = [3, 5, 7];
const PAD_MIN = 0;
const PAD_MAX = 15;

// Measured, not guessed. The shortest gap in a real event shoot was 0.18s and
// the number of bursts found peaks around a second; see `report_burst_thresholds`.
const GAP_MIN = 0.2;
const GAP_MAX = 3.0;
const GAP_STEP = 0.05;

export default function AutoStackModal({
  isOpen,
  targetPaths,
  mode = 'brackets',
  onClose,
  onApply,
}: AutoStackModalProps) {
  const { t } = useTranslation();
  const [isMounted, setIsMounted] = useState(false);
  const [show, setShow] = useState(false);

  const isBursts = mode === 'bursts';
  const keys = isBursts ? 'modals.burstStack' : 'modals.autoStack';

  const [sizes, setSizes] = useState<Array<number>>([3, 5, 7]);
  const [padSeconds, setPadSeconds] = useState(3);
  const [gapSeconds, setGapSeconds] = useState(1.0);
  const [preview, setPreview] = useState<AutoStackPreview | null>(null);
  const [isAnalyzing, setIsAnalyzing] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Analysis reads metadata off disk, so a fast slider drag would otherwise
  // queue a request per pixel. Only the newest result is allowed to land.
  const requestId = useRef(0);

  useEffect(() => {
    if (isOpen) {
      setIsMounted(true);
      const timer = setTimeout(() => setShow(true), 10);
      return () => clearTimeout(timer);
    }
    setShow(false);
    const timer = setTimeout(() => {
      setIsMounted(false);
      setPreview(null);
      setError(null);
    }, 300);
    return () => clearTimeout(timer);
  }, [isOpen]);

  useEffect(() => {
    if (!isOpen || targetPaths.length === 0) {
      return;
    }

    const id = ++requestId.current;
    setIsAnalyzing(true);
    setError(null);

    const timer = setTimeout(() => {
      const request = isBursts
        ? invoke<AutoStackPreview>('preview_burst_stacks', {
            paths: targetPaths,
            params: { maxGapSeconds: gapSeconds, minFrames: 2, exposureToleranceEv: 0.01 },
          })
        : invoke<AutoStackPreview>('preview_auto_stacks', {
            paths: targetPaths,
            params: {
              sizes,
              baseGapSeconds: padSeconds,
              exposureGapFactor: 2.5,
              medianToleranceEv: 0.01,
            },
          });

      request
        .then((result) => {
          if (id !== requestId.current) return;
          setPreview(result);
        })
        .catch((err) => {
          if (id !== requestId.current) return;
          setError(String(err));
        })
        .finally(() => {
          if (id !== requestId.current) return;
          setIsAnalyzing(false);
        });
    }, 150);

    return () => clearTimeout(timer);
  }, [isOpen, targetPaths, sizes, padSeconds, gapSeconds, isBursts]);

  const toggleSize = useCallback((size: number) => {
    setSizes((current) =>
      current.includes(size) ? current.filter((s) => s !== size) : [...current, size].sort((a, b) => a - b),
    );
  }, []);

  const stackedFrameCount = useMemo(
    () => preview?.stacks.reduce((total, stack) => total + stack.paths.length, 0) ?? 0,
    [preview],
  );

  const canApply = (preview?.stacks.length ?? 0) > 0 && !isAnalyzing;

  if (!isMounted) {
    return null;
  }

  return (
    <div
      aria-modal="true"
      className={`fixed inset-0 flex items-center justify-center z-50 bg-black/30 backdrop-blur-xs transition-opacity duration-300 ease-in-out ${
        show ? 'opacity-100' : 'opacity-0'
      }`}
      onClick={onClose}
      role="dialog"
    >
      <div
        className={`bg-surface rounded-lg shadow-xl p-6 w-full max-w-md transform transition-all duration-300 ease-out ${
          show ? 'scale-100 opacity-100 translate-y-0' : 'scale-95 opacity-0 -translate-y-4'
        }`}
        onClick={(e: any) => e.stopPropagation()}
      >
        <Text variant={TextVariants.title} className="mb-1">
          {t(`${keys}.title`)}
        </Text>
        <Text variant={TextVariants.small} className="text-text-secondary mb-5">
          {t(`${keys}.subtitle`, { count: targetPaths.length })}
        </Text>

        {isBursts ? null : (
          <>
            <Text variant={TextVariants.small} className="uppercase text-text-secondary mb-2">
              {t('modals.autoStack.bracketSizes')}
            </Text>
            <div className="flex gap-2 mb-1">
              {SELECTABLE_SIZES.map((size) => {
                const active = sizes.includes(size);
                return (
                  <button
                    key={size}
                    onClick={() => toggleSize(size)}
                    className={`flex-1 h-10 rounded-md border transition-colors ${
                      active
                        ? 'bg-accent text-button-text border-accent font-semibold'
                        : 'bg-bg-primary text-text-secondary border-border hover:text-text-primary'
                    }`}
                  >
                    {t('modals.autoStack.frameCount', { count: size })}
                  </button>
                );
              })}
            </div>
            <Text variant={TextVariants.small} className="text-text-secondary mb-5">
              {t('modals.autoStack.oddOnly')}
            </Text>
          </>
        )}

        <div className="flex items-baseline justify-between mb-2">
          <Text variant={TextVariants.small} className="uppercase text-text-secondary">
            {isBursts ? t('modals.burstStack.maxGap') : t('modals.autoStack.extraTime')}
          </Text>
          <Text variant={TextVariants.small} className="tabular-nums">
            {isBursts ? `${gapSeconds.toFixed(2)}s` : `${padSeconds}s`}
          </Text>
        </div>
        <input
          type="range"
          min={isBursts ? GAP_MIN : PAD_MIN}
          max={isBursts ? GAP_MAX : PAD_MAX}
          step={isBursts ? GAP_STEP : 1}
          value={isBursts ? gapSeconds : padSeconds}
          onChange={(e: any) =>
            isBursts ? setGapSeconds(Number(e.target.value)) : setPadSeconds(Number(e.target.value))
          }
          className="w-full accent-accent cursor-pointer"
        />
        <Text variant={TextVariants.small} className="text-text-secondary mt-1 mb-5">
          {isBursts ? t('modals.burstStack.maxGapHint') : t('modals.autoStack.extraTimeHint')}
        </Text>

        <div className="bg-bg-primary rounded-md border border-border p-3 min-h-[92px] flex flex-col justify-center">
          {error ? (
            <Text variant={TextVariants.small} className="text-red-400">
              {error}
            </Text>
          ) : isAnalyzing && !preview ? (
            <Text variant={TextVariants.small} className="text-text-secondary">
              {t(`${keys}.analyzing`)}
            </Text>
          ) : preview ? (
            <div className={`flex flex-col gap-1 transition-opacity ${isAnalyzing ? 'opacity-50' : 'opacity-100'}`}>
              {Object.keys(preview.sizeCounts).length === 0 ? (
                <Text variant={TextVariants.small} className="text-text-secondary">
                  {t(`${keys}.noneFound`)}
                </Text>
              ) : (
                Object.entries(preview.sizeCounts)
                  .sort((a, b) => Number(a[0]) - Number(b[0]))
                  .map(([size, count]) => (
                    <div key={size} className="flex justify-between">
                      <Text variant={TextVariants.small}>{t(`${keys}.frameStacks`, { size })}</Text>
                      <Text variant={TextVariants.small} className="tabular-nums font-semibold">
                        {count}
                      </Text>
                    </div>
                  ))
              )}
              <div className="flex justify-between border-t border-border mt-1 pt-1">
                <Text variant={TextVariants.small} className="text-text-secondary">
                  {t(`${keys}.leftOver`)}
                </Text>
                <Text variant={TextVariants.small} className="tabular-nums text-text-secondary">
                  {preview.ungrouped.length}
                </Text>
              </div>
            </div>
          ) : null}
        </div>

        <div className="flex justify-end gap-3 mt-5">
          <button
            className="px-4 py-2 rounded-md text-text-secondary hover:bg-surface transition-colors"
            onClick={onClose}
          >
            {t('modals.autoStack.cancel')}
          </button>
          <button
            className="px-4 py-2 rounded-md bg-accent text-button-text font-semibold hover:bg-accent-hover disabled:bg-gray-500 disabled:text-white disabled:cursor-not-allowed transition-colors"
            disabled={!canApply}
            onClick={() => {
              if (preview) {
                onApply(preview.stacks);
              }
              onClose();
            }}
          >
            {t(`${keys}.apply`, {
              stacks: preview?.stacks.length ?? 0,
              frames: stackedFrameCount,
            })}
          </button>
        </div>
      </div>
    </div>
  );
}
