import { useTranslation } from 'react-i18next';
import { Image as ImageIcon } from 'lucide-react';
import { useLibraryStore } from '../../../store/useLibraryStore';
import { useProcessStore } from '../../../store/useProcessStore';
import { useEditorStore } from '../../../store/useEditorStore';
import Text from '../../ui/Text';
import { TextVariants } from '../../../types/typography';

/**
 * A big look at whichever frame the pointer is over.
 *
 * Lightroom's navigator does two jobs and this is the second of them, which is
 * the one that gets used: hover a frame in the grid or the strip and see it at
 * panel size without leaving the one you are working on. That makes exposure
 * and white balance across a set checkable by flicking along the strip, which
 * is otherwise a lot of opening and closing.
 *
 * The first job, the rectangle showing where the view is zoomed to, is not here
 * yet. It needs the editor's zoom and pan, which drive the GPU view, and that
 * is a bigger piece; see the backlog.
 *
 * What it draws is the thumbnail the grid and the strip are already showing, so
 * hovering costs nothing: the picture is in memory before the pointer arrives.
 * That does cap it at thumbnail resolution, which is enough for the exposure and
 * colour check it exists for and not enough to judge focus.
 */
export default function NavigatorPanel() {
  const { t } = useTranslation();
  const hoveredPath = useLibraryStore((state) => state.hoveredPath);
  const selectedImage = useEditorStore((state) => state.selectedImage);
  const libraryActivePath = useLibraryStore((state) => state.libraryActivePath);

  // Falls back to whatever is being worked on, so the panel is never blank just
  // because the pointer is somewhere else.
  const path = hoveredPath ?? selectedImage?.path ?? libraryActivePath ?? null;
  const thumbnail = useProcessStore((state) => (path ? state.thumbnails[path] : undefined));

  const filename = path ? (path.split('?')[0].split(/[\\/]/).pop() ?? '') : '';

  return (
    <div className="h-full w-full flex flex-col min-h-0 p-3 gap-2">
      <div className="flex-1 min-h-0 rounded-md bg-bg-primary border border-surface overflow-hidden flex items-center justify-center">
        {thumbnail ? (
          <img alt={filename} className="max-h-full max-w-full object-contain" src={thumbnail} />
        ) : (
          <ImageIcon size={28} className="text-text-secondary" />
        )}
      </div>
      <div className="shrink-0 flex items-baseline justify-between gap-2 min-w-0">
        <Text variant={TextVariants.small} className="truncate text-text-secondary">
          {filename || t('editor.navigator.empty')}
        </Text>
        {hoveredPath && (
          <Text variant={TextVariants.small} className="shrink-0 text-text-secondary opacity-70">
            {t('editor.navigator.hovering')}
          </Text>
        )}
      </div>
    </div>
  );
}
