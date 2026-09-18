import { type PointerEvent as ReactPointerEvent, useCallback } from 'react';
import { useShallow } from 'zustand/react/shallow';
import { Minus, Plus } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import Waveform from '../editor/Waveform';
import { useEditorStore } from '../../../store/useEditorStore';
import { useSettingsStore } from '../../../store/useSettingsStore';
import { useWaveformControls } from '../../../hooks/useWaveformControls';
import { Adjustments, DisplayMode } from '../../../utils/adjustments';
import { clampScopeHeight, scopeHeightsFor, withScopeHeight } from '../../../utils/scopeColumn';
import Text from '../../ui/Text';
import { TextVariants } from '../../../types/typography';

/**
 * The scopes as a panel of their own, stacked in a column.
 *
 * They used to live inside the Adjustments panel, above the sliders, with a
 * button to fold them away, and a second copy inside Masks for when Adjustments
 * was not the panel on screen. Two copies of the same thing, each tied to
 * whichever panel happened to be showing, and neither could be put anywhere
 * else. As a panel they inherit everything the layout already does: drag them
 * to any of the four regions, sit them under the sliders or on the other side
 * of the window, and the region's own resizer sets their height.
 *
 * More than one at a time, because judging an image usually wants two of them
 * side by side, a parade against a histogram. Each still switches type on its
 * own by hovering it, so the plus adds one and the type is chosen after.
 *
 * Showing three costs one pass over the pixels, not three: the channel list
 * goes to the backend as one string and `calculate_waveform_from_image`
 * allocates bins for whatever is asked for inside a single loop. Ordering the
 * list is free; adding to it is not, so the minus is worth having.
 *
 * The waveform is expensive enough that the backend only computes it when
 * asked, so what counts as "asked" moved with the scopes: `useImageProcessing`
 * watches whether this panel is the active one in any region, where it used to
 * watch a visibility flag belonging to the panel that contained it.
 */

/** What the plus offers next, in the order it offers them. */
const SCOPE_ORDER: Array<string> = [
  DisplayMode.Luma,
  DisplayMode.Rgb,
  DisplayMode.Parade,
  DisplayMode.Vectorscope,
  DisplayMode.Histogram,
];

/** Two is comfortable, five is every kind at once and the point of a limit. */
const MAX_SCOPES = 5;

/** What the gain button offers, in the order it offers them. */
const VECTORSCOPE_GAINS = [1, 2, 3, 4];

export default function ScopesPanel() {
  const { t } = useTranslation();
  const { setWaveformChannels, setVectorscopeGain, setScopeHeights } = useWaveformControls();
  const theme = useSettingsStore((state) => state.appSettings?.theme);

  const { adjustments, setAdjustments, waveform, histogram, waveformChannels, vectorscopeGain, savedHeights } =
    useEditorStore(
      useShallow((state: any) => ({
        adjustments: state.adjustments,
        setAdjustments: state.setAdjustments,
        waveform: state.waveform,
        histogram: state.histogram,
        waveformChannels: state.waveformChannels,
        vectorscopeGain: state.vectorscopeGain,
        savedHeights: state.scopeHeights,
      })),
    );

  const scopes: Array<string> = waveformChannels?.length ? waveformChannels : [DisplayMode.Luma];
  const heights = scopeHeightsFor(scopes.length, savedHeights);

  const cycleVectorscopeGain = () => {
    const at = VECTORSCOPE_GAINS.indexOf(vectorscopeGain ?? 1);
    setVectorscopeGain(VECTORSCOPE_GAINS[(at + 1) % VECTORSCOPE_GAINS.length]);
  };

  /**
   * Dragging the handle under a scope changes that scope and nothing else.
   *
   * The column scrolls, so the scopes below simply move down. Taking the height
   * off the next one instead would mean a scope you were not touching changing
   * size, which is what makes a handle feel like it is fighting you.
   */
  const startResize = useCallback(
    (index: number) => (event: ReactPointerEvent<HTMLDivElement>) => {
      if (event.pointerType === 'mouse' && event.button !== 0) return;
      event.preventDefault();
      event.stopPropagation();

      const pointerId = event.pointerId;
      const handle = event.currentTarget;
      const startY = event.clientY;
      const readHeights = () => scopeHeightsFor(scopes.length, useEditorStore.getState().scopeHeights);
      const startHeight = readHeights()[index];
      const previousUserSelect = document.documentElement.style.userSelect;
      const previousCursor = document.documentElement.style.cursor;

      handle.setPointerCapture?.(pointerId);
      document.documentElement.style.userSelect = 'none';
      document.documentElement.style.cursor = 'row-resize';

      let latest = startHeight;

      const onMove = (move: PointerEvent) => {
        if (move.pointerId !== pointerId) return;
        move.preventDefault();
        latest = clampScopeHeight(startHeight + (move.clientY - startY));
        // Straight into the store while dragging, so the scope follows the
        // pointer rather than a round trip through the settings file.
        useEditorStore.getState().setEditor({ scopeHeights: withScopeHeight(readHeights(), index, latest) });
      };

      const onUp = (up: PointerEvent) => {
        if (up.pointerId !== pointerId) return;
        if (handle.hasPointerCapture?.(pointerId)) handle.releasePointerCapture(pointerId);
        document.documentElement.style.userSelect = previousUserSelect;
        document.documentElement.style.cursor = previousCursor;
        document.removeEventListener('pointermove', onMove);
        document.removeEventListener('pointerup', onUp);
        document.removeEventListener('pointercancel', onUp);
        // One write to disk for the whole drag, not one per frame.
        setScopeHeights(withScopeHeight(readHeights(), index, latest));
      };

      document.addEventListener('pointermove', onMove, { passive: false });
      document.addEventListener('pointerup', onUp);
      document.addEventListener('pointercancel', onUp);
    },
    [scopes.length, setScopeHeights],
  );

  const addScope = () => {
    // The first kind not already up, so pressing plus twice gives two different
    // scopes rather than the same one twice.
    const next = SCOPE_ORDER.find((mode) => !scopes.includes(mode)) ?? DisplayMode.Luma;
    setWaveformChannels([...scopes, next]);
  };

  const removeScope = () => setWaveformChannels(scopes.slice(0, -1));

  const setScopeAt = (index: number, mode: string) =>
    setWaveformChannels(scopes.map((current, i) => (i === index ? mode : current)));

  const toggleClipping = () =>
    setAdjustments((prev: Adjustments) => ({ ...prev, showClipping: !prev.showClipping }));

  return (
    <div className="h-full w-full flex flex-col min-h-0">
      <div className="shrink-0 flex items-center justify-between px-3 pt-3 pb-1">
        <Text variant={TextVariants.small} className="uppercase text-text-secondary">
          {t('editor.scopes.count', { count: scopes.length })}
        </Text>
        <div className="flex items-center gap-1">
          <button
            className="w-7 h-7 flex items-center justify-center rounded-md text-text-secondary hover:bg-surface hover:text-text-primary disabled:opacity-40 disabled:cursor-not-allowed transition-colors"
            onClick={removeScope}
            disabled={scopes.length <= 1}
            data-tooltip={t('editor.scopes.remove')}
          >
            <Minus size={16} />
          </button>
          <button
            className="w-7 h-7 flex items-center justify-center rounded-md text-text-secondary hover:bg-surface hover:text-text-primary disabled:opacity-40 disabled:cursor-not-allowed transition-colors"
            onClick={addScope}
            disabled={scopes.length >= MAX_SCOPES}
            data-tooltip={t('editor.scopes.add')}
          >
            <Plus size={16} />
          </button>
        </div>
      </div>

      {/* Scrolls rather than squeezing: four scopes in a short region would each
          be too flat to read, and a scope you cannot read is not worth drawing. */}
      <div className="flex-1 min-h-0 overflow-y-auto custom-scrollbar px-3 pb-3 flex flex-col">
        {scopes.map((mode, index) => (
          <div key={`${mode}-${index}`} className="shrink-0 flex flex-col">
            <div className="shrink-0" style={{ height: heights[index] }}>
              <Waveform
                waveformData={waveform || null}
                histogram={histogram}
                displayMode={mode}
                setDisplayMode={(next: string) => setScopeAt(index, next)}
                showClipping={adjustments?.showClipping || false}
                onToggleClipping={toggleClipping}
                theme={theme}
                vectorscopeGain={vectorscopeGain ?? 1}
                onCycleVectorscopeGain={cycleVectorscopeGain}
              />
            </div>
            {/* A handle under every scope, the last one included, so one scope
                can be made taller than the panel and scrolled to. The bar only
                appears on hover; the grab area is taller than the bar, because
                a two pixel target is a fight. */}
            <div
              onPointerDown={startResize(index)}
              className="group h-2.5 shrink-0 cursor-row-resize flex items-center justify-center touch-none"
              data-tooltip={t('editor.scopes.resize')}
            >
              <div className="h-0.5 w-10 rounded-full bg-transparent group-hover:bg-text-secondary/50 transition-colors" />
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
