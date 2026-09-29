import React, { useState, useEffect, useRef, useCallback, useMemo } from 'react';
import { List, useListCallbackRef } from 'react-window';
import { ChevronUp, ChevronDown } from 'lucide-react';
import debounce from 'lodash.debounce';
import { useTranslation } from 'react-i18next';
import { Row } from './LibraryItems';
import { useShallow } from 'zustand/react/shallow';
import { useLibraryStore } from '../../../store/useLibraryStore';
import { useStackToggle } from '../../../hooks/useStackToggle';
import {
  LibraryViewMode,
  SortDirection,
  LibraryDisplayMode,
  THUMBNAIL_SIZE_DEFAULT,
  THUMBNAIL_SIZE_MAX,
  THUMBNAIL_SIZE_MIN,
  THUMBNAIL_SIZE_STEP,
} from '../../ui/AppProperties';
import Text from '../../ui/Text';
import { TextColors, TextVariants, TextWeights, TEXT_COLOR_KEYS } from '../../../types/typography';
import { useProcessStore } from '../../../store/useProcessStore';
import { ExifOverlay } from '../../ui/AppProperties';
import { useSettingsStore } from '../../../store/useSettingsStore';

function ListHeader({ widths, setWidths, containerRef, sortCriteria, onSortChange }: any) {
  const { t } = useTranslation();
  const exifOverlay = useSettingsStore((s) => s.appSettings?.exifOverlay || ExifOverlay.Off);
  const showExifCols = exifOverlay !== ExifOverlay.Off;
  const totalRawWidth =
    widths.thumbnail +
    widths.name +
    widths.date +
    widths.rating +
    widths.color +
    (showExifCols ? widths.shutter + widths.aperture + widths.iso + widths.focal : 0);

  const handleResize = (e: React.MouseEvent, leftCol: string, rightCol: string) => {
    e.preventDefault();
    e.stopPropagation();
    const startX = e.clientX;
    const startLeftWidth = widths[leftCol];
    const startRightWidth = widths[rightCol];
    const containerWidth = containerRef.current?.clientWidth || 1000;

    const onMouseMove = (moveEvent: MouseEvent) => {
      const deltaX = moveEvent.clientX - startX;
      const deltaPercent = (deltaX / containerWidth) * 100;

      let newLeft = startLeftWidth + deltaPercent;
      let newRight = startRightWidth - deltaPercent;

      if (newLeft < 1) {
        newRight -= 1 - newLeft;
        newLeft = 1;
      }
      if (newRight < 1) {
        newLeft -= 1 - newRight;
        newRight = 1;
      }

      setWidths((prev: any) => ({
        ...prev,
        [leftCol]: newLeft,
        [rightCol]: newRight,
      }));
    };

    const onMouseUp = () => {
      document.removeEventListener('mousemove', onMouseMove);
      document.removeEventListener('mouseup', onMouseUp);
    };

    document.addEventListener('mousemove', onMouseMove);
    document.addEventListener('mouseup', onMouseUp);
  };

  const Column = ({ title, widthKey, nextKey, sortKey }: any) => {
    const isSorted = sortCriteria.key === sortKey;
    const isAsc = sortCriteria.order === SortDirection.Ascending;
    const actualWidth = `${(widths[widthKey] / totalRawWidth) * 100}%`;

    return (
      <div
        style={{ width: actualWidth }}
        className={`relative flex items-center px-3 h-full select-none ${
          sortKey ? 'cursor-pointer hover:bg-bg-primary/50 transition-colors' : ''
        }`}
        onClick={() => sortKey && onSortChange(sortKey)}
      >
        <Text
          variant={TextVariants.small}
          weight={TextWeights.semibold}
          color={isSorted ? TextColors.primary : TextColors.secondary}
          className="uppercase tracking-wider text-[11px]"
        >
          {title}
        </Text>
        {isSorted && (
          <span className={`ml-1 flex items-center ${TEXT_COLOR_KEYS[TextColors.primary]}`}>
            {isAsc ? <ChevronUp size={12} /> : <ChevronDown size={12} />}
          </span>
        )}
        {nextKey && (
          <div
            className="absolute right-[-3px] top-1.5 bottom-1.5 w-[6px] cursor-col-resize z-10 group flex items-center justify-center"
            onMouseDown={(e) => handleResize(e, widthKey, nextKey)}
          >
            <div className="w-px h-full bg-border-color/40 group-hover:bg-accent transition-colors" />
          </div>
        )}
      </div>
    );
  };

  return (
    <div className="flex items-center w-full h-9 bg-bg-secondary/80 backdrop-blur-sm border-b border-border-color/50 shrink-0">
      <Column title="" widthKey="thumbnail" nextKey="name" />
      <Column title={t('library.grid.columns.name')} widthKey="name" nextKey="date" sortKey="name" />
      <Column title={t('library.grid.columns.modified')} widthKey="date" nextKey="rating" sortKey="date" />
      <Column title={t('library.grid.columns.rating')} widthKey="rating" nextKey="color" sortKey="rating" />
      {showExifCols ? (
        <>
          <Column title={t('library.grid.columns.label')} widthKey="color" nextKey="shutter" />
          <Column
            title={t('library.grid.columns.shutter')}
            widthKey="shutter"
            nextKey="aperture"
            sortKey="shutter_speed"
          />
          <Column title={t('library.grid.columns.aperture')} widthKey="aperture" nextKey="iso" sortKey="aperture" />
          <Column title={t('library.grid.columns.iso')} widthKey="iso" nextKey="focal" sortKey="iso" />
          <Column title={t('library.grid.columns.focal')} widthKey="focal" sortKey="focal_length" />
        </>
      ) : (
        <Column title={t('library.grid.columns.label')} widthKey="color" />
      )}
    </div>
  );
}

const groupImagesByFolder = (images: any[], baseFolderPath: string | null) => {
  const groups: Record<string, any[]> = {};

  images.forEach((img) => {
    const physicalPath = img.path.split('?vc=')[0];
    const separator = physicalPath.includes('/') ? '/' : '\\';
    const lastSep = physicalPath.lastIndexOf(separator);
    const dir = lastSep > -1 ? physicalPath.substring(0, lastSep) : physicalPath;

    if (!groups[dir]) {
      groups[dir] = [];
    }
    groups[dir].push(img);
  });

  const sortedKeys = Object.keys(groups).sort((a, b) => {
    if (a === baseFolderPath) return -1;
    if (b === baseFolderPath) return 1;
    return a.localeCompare(b);
  });

  return sortedKeys.map((dir) => ({
    path: dir,
    images: groups[dir],
  }));
};

export default function LibraryGrid(props: any) {
  const {
    imageList,
    libraryViewMode,
    thumbnailSize,
    libraryDisplayMode,
    currentFolderPath,
    activePath,
    multiSelectedPaths,
    onContextMenu,
    onImageClick,
    onImageDoubleClick,
    thumbnailAspectRatio,
    imageRatings,
    onRequestThumbnails,
    onThumbnailSizeChange,
    groupBadgeInfo,
    stackInfo,
    rawCompression,
  } = props;
  const { listColumnWidths, setLibrary, sortCriteria, setSortCriteria, scrollRequest } = useLibraryStore(
    useShallow((state) => ({
      listColumnWidths: state.listColumnWidths,
      setLibrary: state.setLibrary,
      sortCriteria: state.sortCriteria,
      setSortCriteria: state.setSortCriteria,
      scrollRequest: state.scrollRequest,
    })),
  );

  const { toggleStack: handleToggleStack } = useStackToggle();

  const [gridSize, setGridSize] = useState({ height: 0, width: 0 });
  const [listHandle, setListHandle] = useListCallbackRef();
  const [collapsedRecursiveFolders, setCollapsedRecursiveFolders] = useState<Set<string>>(new Set());
  const libraryContainerRef = useRef<HTMLDivElement>(null);
  const gridObserverRef = useRef<ResizeObserver | null>(null);
  const loadedThumbnailsRef = useRef(new Set<string>());
  const requestQueueRef = useRef<Set<string>>(new Set());
  const requestTimeoutRef = useRef<any>(null);
  const exifOverlay = useSettingsStore((s) => s.appSettings?.exifOverlay || ExifOverlay.Off);
  const showExifCols = exifOverlay !== ExifOverlay.Off;

  useEffect(() => {
    const el = libraryContainerRef.current;
    if (gridObserverRef.current) {
      gridObserverRef.current.disconnect();
      gridObserverRef.current = null;
    }
    if (el) {
      const ro = new ResizeObserver((entries) => {
        const entry = entries[0];
        if (entry) {
          const height = Math.round(entry.contentRect.height);
          const width = Math.round(entry.contentRect.width);

          setGridSize((prev) => (prev.height === height && prev.width === width ? prev : { height, width }));
        }
      });
      ro.observe(el);
      gridObserverRef.current = ro;
    }
    return () => gridObserverRef.current?.disconnect();
  }, [libraryContainerRef]);

  // BLITZRAW: a Mac trackpad or a pinch sends dozens of small wheel events for
  // one gesture, where a mouse sends one per notch. On a Mac they are added up
  // and the size moves once per notch's worth, or one flick would race from
  // the smallest size to the largest and keep going after the fingers lift.
  const wheelTotalRef = useRef(0);

  useEffect(() => {
    const WHEEL_NOTCH_PIXELS = 100;
    const handleWheel = (event: any) => {
      const container = libraryContainerRef.current;
      if (!container || !container.contains(event.target)) {
        return;
      }

      if (event.ctrlKey || event.metaKey) {
        event.preventDefault();
        // One step of the slider per notch, rather than one of three sizes.
        let steps = event.deltaY < 0 ? 1 : -1;
        if (useSettingsStore.getState().osPlatform === 'macos') {
          wheelTotalRef.current += event.deltaMode === 1 ? event.deltaY * 16 : event.deltaY;
          const notches = Math.trunc(wheelTotalRef.current / WHEEL_NOTCH_PIXELS);
          if (notches === 0) {
            return;
          }
          wheelTotalRef.current -= notches * WHEEL_NOTCH_PIXELS;
          steps = -notches;
        }
        const next = Math.min(
          THUMBNAIL_SIZE_MAX,
          Math.max(THUMBNAIL_SIZE_MIN, thumbnailSize + steps * THUMBNAIL_SIZE_STEP),
        );
        if (next !== thumbnailSize) {
          onThumbnailSizeChange(next);
        }
      }
    };

    window.addEventListener('wheel', handleWheel, { passive: false });
    return () => {
      window.removeEventListener('wheel', handleWheel);
    };
  }, [thumbnailSize, onThumbnailSizeChange]);

  const handleScroll = useMemo(
    () =>
      debounce((top: number) => {
        setLibrary({ libraryScrollTop: top });
      }, 200),
    [setLibrary],
  );

  useEffect(() => () => handleScroll.cancel(), [handleScroll]);

  const queueThumbnailRequest = useCallback(
    (path: string) => {
      if (!onRequestThumbnails) return;
      if (useProcessStore.getState().thumbnails[path]) return;
      requestQueueRef.current.add(path);
      if (!requestTimeoutRef.current) {
        requestTimeoutRef.current = setTimeout(() => {
          const paths = Array.from(requestQueueRef.current);
          if (paths.length > 0) {
            onRequestThumbnails(paths);
            requestQueueRef.current.clear();
          }
          requestTimeoutRef.current = null;
        }, 50);
      }
    },
    [onRequestThumbnails],
  );

  const handleToggleRecursiveFolder = useCallback((path: string) => {
    setCollapsedRecursiveFolders((prev) => {
      const next = new Set(prev);
      next.has(path) ? next.delete(path) : next.add(path);
      return next;
    });
  }, []);

  const handleImageLoad = useCallback((path: string) => {
    loadedThumbnailsRef.current.add(path);
  }, []);

  const gridData = useMemo(() => {
    if (gridSize.width === 0 || imageList.length === 0) return null;

    const isListView = libraryDisplayMode === LibraryDisplayMode.List;
    const OUTER_PADDING = isListView ? 0 : 12;
    const ITEM_GAP = isListView ? 0 : 12;
    const minThumbWidth = thumbnailSize || THUMBNAIL_SIZE_DEFAULT;

    const availableWidth = gridSize.width - OUTER_PADDING * 2;
    const columnCount = isListView
      ? 1
      : Math.max(1, Math.floor((availableWidth + ITEM_GAP) / (minThumbWidth + ITEM_GAP)));
    const itemWidth = isListView ? availableWidth : (availableWidth - ITEM_GAP * (columnCount - 1)) / columnCount;

    const totalBase =
      listColumnWidths.thumbnail +
      listColumnWidths.name +
      listColumnWidths.date +
      listColumnWidths.rating +
      listColumnWidths.color +
      (showExifCols
        ? listColumnWidths.shutter + listColumnWidths.aperture + listColumnWidths.iso + listColumnWidths.focal
        : 0);

    const listRowHeight = Math.max(36, Math.min(300, (availableWidth * listColumnWidths.thumbnail) / totalBase));
    const rowHeight = isListView ? listRowHeight : itemWidth + ITEM_GAP;
    const headerHeight = 40;

    const rows: any[] = [];

    if (libraryViewMode === LibraryViewMode.Recursive) {
      const groups = groupImagesByFolder(imageList, currentFolderPath);
      groups.forEach((group) => {
        if (group.images.length === 0) return;

        const isExpanded = !collapsedRecursiveFolders.has(group.path);
        rows.push({ type: 'header', path: group.path, count: group.images.length, isExpanded });

        if (isExpanded) {
          for (let i = 0; i < group.images.length; i += columnCount) {
            rows.push({
              type: 'images',
              images: group.images.slice(i, i + columnCount),
              startIndex: i,
            });
          }
        }
      });
    } else {
      for (let i = 0; i < imageList.length; i += columnCount) {
        rows.push({
          type: 'images',
          images: imageList.slice(i, i + columnCount),
          startIndex: i,
        });
      }
    }

    rows.push({ type: 'footer' });

    return {
      rows,
      itemWidth,
      rowHeight,
      listRowHeight,
      OUTER_PADDING,
      ITEM_GAP,
      columnCount,
      isListView,
      headerHeight,
    };
  }, [
    gridSize.width,
    imageList,
    libraryViewMode,
    libraryDisplayMode,
    collapsedRecursiveFolders,
    thumbnailSize,
    listColumnWidths.thumbnail,
    currentFolderPath,
  ]);

  useEffect(() => {
    if (!listHandle?.element || !gridData) return;

    const savedTop = useLibraryStore.getState().libraryScrollTop;
    const element = listHandle.element as HTMLElement;

    if (savedTop > 0) {
      element.scrollTop = savedTop;
    }
  }, [listHandle, currentFolderPath]);

  const prevActivePath = useRef<string | null>(null);
  const prevDisplayMode = useRef<LibraryDisplayMode | null>(null);
  const prevListElement = useRef<HTMLElement | null>(null);
  // BLITZRAW: where in the list the selected photo sat last time. Turning a
  // filter on or off keeps the same photo selected and moves it hundreds of
  // rows, and the check below was only ever asking whether the photo had
  // changed. It had not, so nothing scrolled, and the view stayed pointed at
  // whatever now happened to occupy those pixels. Position is the thing that
  // actually decides whether a scroll is needed.
  const prevActiveIndex = useRef<number>(-1);

  // BLITZRAW: how far down the list a given photo's row starts, or null when it
  // is not in the list. Lifted out of the effect below so the scroll request
  // effect after it can ask the same question, and so the recursive case, which
  // has to count folder headings as it goes, is written once.
  const rowTopForPath = useCallback(
    (path: string | null): number | null => {
      if (!path || !gridData) return null;

      const { rowHeight, headerHeight, columnCount } = gridData;

      if (libraryViewMode === LibraryViewMode.Recursive) {
        let top = 0;
        const groups = groupImagesByFolder(imageList, currentFolderPath);
        for (const group of groups) {
          if (group.images.length === 0) continue;

          top += headerHeight;

          const imageIndex = group.images.findIndex((img) => img.path === path);
          if (imageIndex !== -1) {
            return top + Math.floor(imageIndex / columnCount) * rowHeight;
          }

          top += Math.ceil(group.images.length / columnCount) * rowHeight;
        }
        return null;
      }

      const index = imageList.findIndex((img: any) => img.path === path);
      return index === -1 ? null : Math.floor(index / columnCount) * rowHeight;
    },
    [gridData, imageList, libraryViewMode, currentFolderPath],
  );

  useEffect(() => {
    if (!listHandle?.element || !gridData || multiSelectedPaths.length > 1) {
      prevActivePath.current = activePath;
      prevDisplayMode.current = libraryDisplayMode;
      if (listHandle?.element) prevListElement.current = listHandle.element as HTMLElement;
      return;
    }

    const element = listHandle.element as HTMLElement;
    const activeIndex = imageList.findIndex((img: any) => img.path === activePath);
    const isPathSame = activePath === prevActivePath.current;
    const isModeSame = libraryDisplayMode === prevDisplayMode.current;
    const isElementSame = element === prevListElement.current;
    const isIndexSame = activeIndex === prevActiveIndex.current;

    if (isPathSame && isModeSame && isElementSame && isIndexSame) return;

    prevActivePath.current = activePath;
    prevDisplayMode.current = libraryDisplayMode;
    prevListElement.current = element;
    prevActiveIndex.current = activeIndex;

    // The photo moved without being reselected, which is a filter or a sort
    // rearranging the list under it. Centring is the right answer there: the
    // eye is already on that photo and it should stay where the eye is.
    const listRearranged = isPathSame && isModeSame && isElementSame && !isIndexSame;

    const targetTop = rowTopForPath(activePath);
    const found = targetTop !== null;
    const { rowHeight } = gridData;

    if (found) {
      const clientHeight = element.clientHeight;
      const scrollTop = element.scrollTop;
      const itemBottom = targetTop + rowHeight;
      const SCROLL_OFFSET = 120;

      if (!isModeSame || !isElementSame || listRearranged) {
        element.scrollTo({
          top: Math.max(0, targetTop - clientHeight / 2 + rowHeight / 2),
          behavior: 'instant',
        });
      } else if (itemBottom > scrollTop + clientHeight) {
        element.scrollTo({
          top: itemBottom - clientHeight + SCROLL_OFFSET,
          behavior: 'smooth',
        });
      } else if (targetTop < scrollTop) {
        element.scrollTo({
          top: Math.max(0, targetTop - SCROLL_OFFSET),
          behavior: 'smooth',
        });
      }
    }
  }, [
    activePath,
    gridData,
    multiSelectedPaths.length,
    listHandle,
    currentFolderPath,
    imageList,
    libraryViewMode,
    libraryDisplayMode,
    rowTopForPath,
  ]);

  // ============== BLITZRAW: following a selection that is still growing ==============
  // Alt and an arrow takes in one more frame per press and leaves the current
  // one where it started, so the effect above, which follows the current frame
  // and gives up entirely once more than one photo is selected, never moves.
  // Without this the frames being taken in walk off the bottom of the screen.
  //
  // Only when the frame is actually off screen, and only far enough to bring it
  // on. Centring every press would swing the whole grid under a gesture that
  // moves by one row at a time.
  const lastScrollRequest = useRef<number>(-1);

  useEffect(() => {
    if (!scrollRequest || !listHandle?.element || !gridData) return;
    if (scrollRequest.id === lastScrollRequest.current) return;
    lastScrollRequest.current = scrollRequest.id;

    const top = rowTopForPath(scrollRequest.path);
    if (top === null) return;

    const element = listHandle.element as HTMLElement;
    const { rowHeight } = gridData;
    const clientHeight = element.clientHeight;
    const scrollTop = element.scrollTop;
    const itemBottom = top + rowHeight;
    const SCROLL_OFFSET = 120;

    // A centred request is reopening a session, not following a gesture. Put
    // the frame in the middle straight away, and record where we left it so the
    // effect above does not decide the view has drifted and scroll again.
    if (scrollRequest.center) {
      element.scrollTo({
        top: Math.max(0, top - clientHeight / 2 + rowHeight / 2),
        behavior: 'instant',
      });
      prevActivePath.current = scrollRequest.path;
      prevListElement.current = element;
      prevActiveIndex.current = imageList.findIndex((img: any) => img.path === scrollRequest.path);
      return;
    }

    if (itemBottom > scrollTop + clientHeight) {
      element.scrollTo({ top: itemBottom - clientHeight + SCROLL_OFFSET, behavior: 'smooth' });
    } else if (top < scrollTop) {
      element.scrollTo({ top: Math.max(0, top - SCROLL_OFFSET), behavior: 'smooth' });
    }
  }, [scrollRequest, listHandle, gridData, rowTopForPath, imageList]);
  // ============ BLITZRAW END: following a selection that is still growing ============

  const memoizedRowProps = useMemo(() => {
    if (!gridData) return {};

    return {
      rows: gridData.rows,
      activePath,
      multiSelectedSet: new Set(multiSelectedPaths),
      onContextMenu,
      onImageClick,
      onImageDoubleClick,
      thumbnailAspectRatio,
      onImageLoad: handleImageLoad,
      imageRatings,
      baseFolderPath: currentFolderPath,
      itemWidth: gridData.itemWidth,
      itemHeight: gridData.isListView ? gridData.listRowHeight : gridData.itemWidth,
      outerPadding: gridData.OUTER_PADDING,
      gap: gridData.ITEM_GAP,
      isListView: gridData.isListView,
      columnWidths: listColumnWidths,
      queueThumbnailRequest,
      onToggleRecursiveFolder: handleToggleRecursiveFolder,
      groupBadgeInfo,
      stackInfo,
      onToggleStack: handleToggleStack,
      rawCompression,
    };
  }, [
    gridData,
    activePath,
    multiSelectedPaths,
    onContextMenu,
    onImageClick,
    onImageDoubleClick,
    thumbnailAspectRatio,
    handleImageLoad,
    imageRatings,
    currentFolderPath,
    listColumnWidths,
    queueThumbnailRequest,
    handleToggleRecursiveFolder,
    groupBadgeInfo,
    stackInfo,
    handleToggleStack,
    rawCompression,
  ]);

  const getItemSize = useCallback(
    (index: number) => {
      if (!gridData) return 0;
      if (gridData.rows[index].type === 'footer') return gridData.isListView ? 24 : gridData.OUTER_PADDING;
      return gridData.rows[index].type === 'header' ? gridData.headerHeight : gridData.rowHeight;
    },
    [gridData],
  );

  if (!gridData) {
    return (
      <div
        ref={libraryContainerRef}
        className="flex-1 w-full h-full"
        onClick={props.onClearSelection}
        onContextMenu={props.onEmptyAreaContextMenu}
      />
    );
  }

  const handleHeaderSort = (key: string) => {
    props.onClearSelection();
    setSortCriteria((prev: any) => {
      if (prev.key === key) {
        if (prev.order === SortDirection.Ascending) {
          return { ...prev, order: SortDirection.Descending };
        } else {
          return { key: 'name', order: SortDirection.Ascending };
        }
      }
      return { key, order: SortDirection.Ascending };
    });
  };

  return (
    <div
      ref={libraryContainerRef}
      className="flex-1 w-full h-full"
      onClick={props.onClearSelection}
      onContextMenu={props.onEmptyAreaContextMenu}
    >
      <div className="flex flex-col w-full h-full">
        {gridData.isListView && (
          <ListHeader
            widths={listColumnWidths}
            setWidths={(w: any) => setLibrary({ listColumnWidths: typeof w === 'function' ? w(listColumnWidths) : w })}
            containerRef={libraryContainerRef}
            sortCriteria={sortCriteria}
            onSortChange={handleHeaderSort}
          />
        )}
        <div style={{ height: gridData.isListView ? gridSize.height - 36 : gridSize.height, width: gridSize.width }}>
          <List
            listRef={setListHandle}
            rowCount={gridData.rows.length}
            rowHeight={getItemSize}
            onScroll={(e: React.UIEvent<HTMLElement>) => handleScroll(e.currentTarget.scrollTop)}
            className="custom-scrollbar"
            rowComponent={Row}
            rowProps={memoizedRowProps}
          />
        </div>
      </div>
    </div>
  );
}
