import { useEffect, useMemo, useRef } from 'react';
import { useShallow } from 'zustand/react/shallow';
import { History as HistoryIcon } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import clsx from 'clsx';
import { useEditorStore } from '../../../store/useEditorStore';
import { describeSteps } from '../../../utils/historyNames';
import Text from '../../ui/Text';
import { TextVariants } from '../../../types/typography';
import { useHistoryJump } from '../../../hooks/useHistoryJump';

/**
 * Every step taken on the open photo, oldest at the top.
 *
 * The same list the clock in the editor toolbar shows, given room. A dropdown
 * holds a name and nothing else, closes the moment you click, and cannot be put
 * anywhere; a panel can be left open beside the photo while you work, which is
 * how a history actually gets used.
 *
 * Not detachable to the floating window, unlike the scopes and the navigator.
 * Clicking a step changes the photo, so this writes rather than only displays,
 * and a write from the second window would have to cross back in order. That is
 * the same line drawn for Adjustments and Masks. See `useDetachPanel`.
 *
 * # What it draws
 *
 * The store keeps the history as states rather than as changes, so a step is
 * named by comparing it against the one before it. `describeSteps` does that,
 * and a step that named itself, like a reset, keeps its own name instead. See
 * `editHistory.ts`.
 *
 * Steps ahead of the current one are the ones you have stepped back over. They
 * are dimmed rather than hidden, because they are still there to go forward
 * into, and they disappear on their own the moment you do something else.
 *
 * # What it does not do yet
 *
 * Nothing here is saved. Close the application and the history goes with it;
 * that is the next phase, along with pinning what was exported and marking a
 * step by hand.
 */
export default function HistoryPanel() {
  const { t } = useTranslation();

  const { history, historyLabels, historyIndex, selectedImage } = useEditorStore(
    useShallow((state: any) => ({
      history: state.history,
      historyLabels: state.historyLabels,
      historyIndex: state.historyIndex,
      selectedImage: state.selectedImage,
    })),
  );

  // BLITZRAW: clicking a row is a thing I did, not an undo, so it goes into the
  // list of what I did and Ctrl+Z takes the jump back. It also moves the
  // photo's own bookmark on disk, so leaving and coming back lands here again.
  // See utils/appActions.ts.
  const goToHistoryIndex = useHistoryJump();

  const names = useMemo(() => describeSteps(history, historyLabels), [history, historyLabels]);

  // Newest first. The list is stored oldest first, because that is the order
  // things happened in and the order every index refers to, so it is reversed
  // for drawing only and each row keeps the index it actually has.
  //
  // Newest first is the right way round for a panel that stays open: what you
  // just did is what you want to see, and it is at the top without scrolling
  // however long the history gets.
  const rows = useMemo(() => names.map((name, index) => ({ name, index })).reverse(), [names]);

  // Follows the current step, so undoing a long way does not leave the panel
  // showing somewhere else entirely.
  const listRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const active = listRef.current?.querySelector('[data-current="true"]');
    active?.scrollIntoView({ block: 'nearest' });
  }, [historyIndex]);

  if (!selectedImage) {
    return (
      <div className="h-full w-full flex flex-col items-center justify-center gap-2 px-6 text-center">
        <HistoryIcon size={24} className="text-text-secondary opacity-60" />
        <Text variant={TextVariants.small} className="text-text-secondary">
          {t('editor.history.noPhoto')}
        </Text>
      </div>
    );
  }

  return (
    <div className="h-full w-full flex flex-col min-h-0">
      <div className="shrink-0 flex items-center justify-between px-3 pt-3 pb-1">
        <Text variant={TextVariants.small} className="uppercase text-text-secondary">
          {t('editor.history.count', { count: history.length })}
        </Text>
      </div>

      <div ref={listRef} className="flex-1 min-h-0 overflow-y-auto custom-scrollbar px-2 pb-3 flex flex-col gap-0.5">
        {rows.map(({ name, index }) => {
          const isCurrent = index === historyIndex;
          // Stepped back over: still reachable by going forward, and gone the
          // moment anything else is done.
          const isAhead = index > historyIndex;

          return (
            <button
              key={index}
              type="button"
              data-current={isCurrent}
              onClick={() => void goToHistoryIndex(index)}
              className={clsx(
                'text-left rounded-md px-2.5 py-1.5 transition-colors flex items-baseline gap-2 min-w-0',
                isCurrent
                  ? 'bg-accent text-button-text'
                  : isAhead
                    ? 'text-text-secondary opacity-50 hover:bg-surface'
                    : 'text-text-primary hover:bg-surface',
              )}
            >
              <span className="shrink-0 tabular-nums text-xs opacity-60 w-5 text-right">{index}</span>
              <span className="truncate text-sm">{name}</span>
            </button>
          );
        })}
      </div>
    </div>
  );
}
