import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import clsx from 'clsx';
import { useShallow } from 'zustand/react/shallow';
import { useLibraryStore } from '../../store/useLibraryStore';
import { useProcessStore } from '../../store/useProcessStore';
import Text from './Text';
import { TextColors, TextVariants } from '../../types/typography';

/**
 * BLITZRAW: the bar that says something is running.
 *
 * It lives in the bottom bar rather than the library header, because the header
 * is gone in full view and the bottom bar is not, and a slowdown you cannot
 * explain is exactly when you want to know why.
 *
 * The sweep is deliberately not progress. Most of what makes the app pause,
 * thumbnail rendering above all, reports its own in bursts or not at all, so a
 * bar creeping towards a percentage would be inventing a number. This claims
 * only that something is running, which is the whole question being asked when
 * the grid goes quiet. The line above it carries a count where one exists.
 *
 * It reads the stores itself rather than taking props, so the one place that
 * draws it does not have to be somewhere the counts already happen to reach.
 */
export default function BusyIndicator() {
  const { t } = useTranslation();

  const isViewLoading = useLibraryStore((state) => state.isViewLoading);
  const { isIndexing, thumbnailProgress, previewProgress } = useProcessStore(
    useShallow((state) => ({
      isIndexing: state.isIndexing,
      thumbnailProgress: state.thumbnailProgress,
      previewProgress: state.previewProgress,
    })),
  );

  const total = thumbnailProgress?.total ?? 0;
  const current = thumbnailProgress?.current ?? 0;
  const isRenderingThumbnails = total > 0 && current < total;

  const previewTotal = previewProgress?.total ?? 0;
  const previewCurrent = previewProgress?.current ?? 0;
  const isBuildingPreviews = previewTotal > 0;

  const isBusy = isViewLoading || isIndexing || isRenderingThumbnails || isBuildingPreviews;

  // Held off for a second before appearing and half a second before leaving, so
  // a folder that opens instantly does not flash a bar on the way past.
  const [isDelayed, setIsDelayed] = useState(false);
  const [isMounted, setIsMounted] = useState(false);

  useEffect(() => {
    const timer = window.setTimeout(() => setIsDelayed(isBusy), isBusy ? 1000 : 500);
    return () => clearTimeout(timer);
  }, [isBusy]);

  useEffect(() => {
    if (isDelayed) {
      setIsMounted(true);
    }
  }, [isDelayed]);

  // Named jobs first, since a count of files is more use than a word, and
  // building previews before all of them because it is the one job here the
  // user started on purpose and by far the longest. The last line is for a
  // pause nothing has claimed, which is still worth showing rather than
  // leaving the app silently slow.
  const message = isBuildingPreviews
    ? t('library.busy.previews', { current: previewCurrent, total: previewTotal })
    : isRenderingThumbnails
      ? t('library.busy.thumbnails', { current, total })
      : isIndexing
        ? t('library.busy.indexing')
        : isViewLoading
          ? t('library.busy.loading')
          : t('library.busy.working');

  return (
    <div
      className={clsx(
        'overflow-hidden transition-all duration-300 pointer-events-none',
        isDelayed ? 'max-w-64 opacity-100' : 'max-w-0 opacity-0',
      )}
      onTransitionEnd={(e) => {
        if (e.propertyName === 'opacity' && !isDelayed) {
          setIsMounted(false);
        }
      }}
    >
      {isMounted && (
        <div className="flex flex-col gap-0.5 w-56">
          <Text
            variant={TextVariants.small}
            color={TextColors.secondary}
            className="whitespace-nowrap leading-none truncate text-[10px] text-center"
          >
            {message}
          </Text>
          <div className="h-1.5 w-full rounded-full bg-accent/80 overflow-hidden relative">
            <div className="absolute inset-0 blitzraw-busy-sweep" />
          </div>
        </div>
      )}
    </div>
  );
}
