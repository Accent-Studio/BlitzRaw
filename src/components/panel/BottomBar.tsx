import { useState, useEffect, useRef, useMemo } from 'react';
import {
  Star,
  Copy,
  ClipboardPaste,
  Check,
  Layers,
  Settings,
  Filter,
  PanelLeft,
  PanelBottom,
  PanelRight,
} from 'lucide-react';
import clsx from 'clsx';
import { motion, AnimatePresence } from 'framer-motion';
import { useShallow } from 'zustand/react/shallow';
import { currentSelectionSummary } from '../../utils/selection';
import { useTranslation } from 'react-i18next';

import Filmstrip from './Filmstrip';
import BusyIndicator from '../ui/BusyIndicator';
import { GLOBAL_KEYS, ImageFile, SelectedImage, ThumbnailAspectRatio } from '../ui/AppProperties';
import { StackInfo } from '../../utils/imageStacking';
import { useStackToggle } from '../../hooks/useStackToggle';
import Text from '../ui/Text';
import { useEditorStore } from '../../store/useEditorStore';
import { useLibraryStore } from '../../store/useLibraryStore';
import { useUIStore } from '../../store/useUIStore';
import { COLOR_LABELS } from '../../utils/adjustments';

interface BottomBarProps {
  filmstripHeight?: number;
  imageList?: Array<ImageFile>;
  imageRatings?: Record<string, number> | null;
  isAndroid?: boolean;
  isCopied: boolean;
  isCopyDisabled: boolean;
  isExportDisabled?: boolean;
  isFilmstripVisible?: boolean;
  isLibraryView?: boolean;
  isLoading?: boolean;
  isPasted: boolean;
  isPasteDisabled: boolean;
  isRatingDisabled?: boolean;
  isResetDisabled?: boolean;
  isResizing?: boolean;
  multiSelectedPaths?: Array<string>;
  /** The stacks in `imageList`, for the strip's badges and the open-all button. */
  stackInfo?: Map<string, StackInfo>;
  onClearSelection?(): void;
  onContextMenu?(event: any, path: string): void;
  onEmptyAreaContextMenu?(event: any): void;
  onCopy(): void;
  onExportClick?(): void;
  onImageSelect?(path: string, event: any): void;
  onOpenCopyPasteSettings?(): void;
  onRequestThumbnails?(paths: string[]): void;
  onPaste(): void;
  onRate(rate: number): void;
  onReset?(): void;
  onZoomChange?(zoomValue: number, fitToWindow?: boolean): void;
  rating: number;
  selectedImage?: SelectedImage;
  setIsFilmstripVisible?(isVisible: boolean): void;
  showFilmstrip?: boolean;
  showZoomControls?: boolean;
  thumbnailAspectRatio: ThumbnailAspectRatio;
  totalImages?: number;
  /**
   * BLITZRAW: how many of `totalImages` survive the current filter and are
   * actually on screen. Absent means nothing is being hidden.
   */
  visibleImages?: number;
}

interface PanelToggleButtonProps {
  onClick: () => void;
  Icon: React.ElementType;
  tooltip: string;
  disabled?: boolean;
}

const PanelToggleButton = ({ onClick, Icon, tooltip, disabled = false }: PanelToggleButtonProps) => (
  <button
    className={clsx(
      'p-1.5 rounded-md transition-colors',
      disabled
        ? 'text-text-secondary opacity-40 cursor-not-allowed'
        : 'text-text-secondary hover:bg-surface hover:text-text-primary',
    )}
    onClick={() => !disabled && onClick()}
    disabled={disabled}
    data-tooltip={tooltip}
  >
    <Icon size={18} />
  </button>
);

export default function BottomBar({
  filmstripHeight,
  imageList = [],
  imageRatings,
  isAndroid,
  isCopied,
  isCopyDisabled,
  isFilmstripVisible,
  isLibraryView = false,
  isLoading = false,
  isPasted,
  isPasteDisabled,
  isRatingDisabled = false,
  isResizing,
  multiSelectedPaths = [],
  onClearSelection,
  onContextMenu,
  onEmptyAreaContextMenu,
  onCopy,
  onImageSelect,
  onOpenCopyPasteSettings,
  onRequestThumbnails,
  onPaste,
  onRate,
  onZoomChange = () => {},
  rating,
  selectedImage,
  stackInfo,
  setIsFilmstripVisible,
  showFilmstrip = true,
  showZoomControls = true,
  thumbnailAspectRatio,
  totalImages,
  visibleImages,
}: BottomBarProps) {
  const { t } = useTranslation();
  const expandedStacks = useLibraryStore((state) => state.expandedStacks);
  const { toggleAllStacks } = useStackToggle();

  const { isInstantTransition, uiVisibility, setUI } = useUIStore(
    useShallow((state) => ({
      isInstantTransition: state.isInstantTransition,
      uiVisibility: state.uiVisibility,
      setUI: state.setUI,
    })),
  );

  const isLeftOpen = uiVisibility.leftPanel;
  const isRightOpen = uiVisibility.rightPanel;
  const isBottomOpen = uiVisibility.filmstrip;

  const toggleLeft = () =>
    setUI((s) => {
      const isOpening = !s.uiVisibility.leftPanel;
      return {
        uiVisibility: { ...s.uiVisibility, leftPanel: isOpening },
        leftPanelWidth: isOpening && s.leftPanelWidth < 250 ? 350 : s.leftPanelWidth,
      };
    });

  const toggleRight = () =>
    setUI((s) => {
      const isOpening = !s.uiVisibility.rightPanel;
      return {
        uiVisibility: { ...s.uiVisibility, rightPanel: isOpening },
        rightPanelWidth: isOpening && s.rightPanelWidth < 250 ? 350 : s.rightPanelWidth,
      };
    });

  const toggleBottom = () =>
    setUI((s) => ({
      uiVisibility: { ...s.uiVisibility, filmstrip: !s.uiVisibility.filmstrip },
    }));

  const { displaySize, originalSize } = useEditorStore(
    useShallow((state) => ({
      displaySize: state.displaySize,
      originalSize: state.originalSize,
    })),
  );

  const [isEditingPercent, setIsEditingPercent] = useState(false);
  const [percentInputValue, setPercentInputValue] = useState('');
  const isDraggingSlider = useRef(false);
  const [isZoomActive, setIsZoomActive] = useState(false);

  const percentInputRef = useRef<HTMLInputElement>(null);
  const [isZoomLabelHovered, setIsZoomLabelHovered] = useState(false);
  const isZoomReady = !isLoading && originalSize && originalSize.width > 0 && displaySize && displaySize.width > 0;

  const currentOriginalPercent = isZoomReady
    ? (displaySize.width * (typeof window !== 'undefined' ? window.devicePixelRatio || 1 : 1)) / originalSize.width
    : 1.0;

  const [latchedSliderValue, setLatchedSliderValue] = useState(1.0);
  const [latchedDisplayPercent, setLatchedDisplayPercent] = useState(100);

  const numSelected = multiSelectedPaths.length;
  const total = totalImages ?? 0;

  // A collapsed stack counts as one click and several files, and which of those
  // two numbers matters depends on what you are about to do. Both are reported:
  // what was picked reads first, because that is what was picked and a selection
  // of eight stacks is eight things rather than thirty, and the file count
  // follows it, because deleting a collapsed stack takes the whole bracket and a
  // bracket quietly reading as one file is how three get deleted by accident.
  const selectionSummary = useMemo(
    () => currentSelectionSummary(multiSelectedPaths),
    [multiSelectedPaths],
  );

  // Opening every stack at once, for a strip or a grid full of them. One button
  // rather than two: with anything still closed it opens, otherwise it closes,
  // which is the same gesture the badge on a single stack already has.
  const stackIds = useMemo(() => [...(stackInfo?.keys() ?? [])], [stackInfo]);
  const anyStackClosed = useMemo(
    () => stackIds.some((id) => !expandedStacks.includes(id)),
    [stackIds, expandedStacks],
  );


  const showSelectionCounter = selectionSummary.files > 1;

  // How much of the folder is on screen: tiles drawn against files found. The
  // two differ for a filter and also for a closed stack, which is one tile over
  // several files, and both are reasons to say so. Equal means nothing is being
  // held back, and the bar gives the size of the folder on its own rather than
  // the same number twice.
  const shown = visibleImages ?? total;
  const isFiltered = shown < total;

  // BLITZRAW: open. The star and colour filters are the first thing reached for
  // on a card of a thousand frames, and a click to reveal them every session is
  // a click for nothing. The button still closes them.
  const [isFilterExpanded, setIsFilterExpanded] = useState(true);
  const { filterCriteria, setFilterCriteria } = useLibraryStore(
    useShallow((state) => ({
      filterCriteria: state.filterCriteria,
      setFilterCriteria: state.setFilterCriteria,
    })),
  );

  const allColors = [...COLOR_LABELS, { name: 'none', color: '#9ca3af' }];
  const currentHeight = filmstripHeight ?? 120;
  const isCollapsed = !isFilmstripVisible;
  const effectiveHeight = isFilmstripVisible ? currentHeight : 0;
  const shouldAnimate = !isInstantTransition && (!isResizing || isCollapsed);

  useEffect(() => {
    if (isZoomReady && !isDraggingSlider.current) {
      setLatchedSliderValue(currentOriginalPercent);
      setLatchedDisplayPercent(Math.round(currentOriginalPercent * 100));
    }
  }, [currentOriginalPercent, isZoomReady]);

  useEffect(() => {
    const handleDragEndGlobal = () => {
      if (isZoomActive) {
        setIsZoomActive(false);
        isDraggingSlider.current = false;
        if (isZoomReady) {
          setLatchedDisplayPercent(Math.round(currentOriginalPercent * 100));
        }
      }
    };

    if (isZoomActive) {
      window.addEventListener('mouseup', handleDragEndGlobal);
      window.addEventListener('touchend', handleDragEndGlobal);
    }

    return () => {
      window.removeEventListener('mouseup', handleDragEndGlobal);
      window.removeEventListener('touchend', handleDragEndGlobal);
    };
  }, [isZoomActive, isZoomReady, currentOriginalPercent]);

  const handleSliderChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const newZoom = parseFloat(e.target.value);
    setLatchedSliderValue(newZoom);
    setLatchedDisplayPercent(Math.round(newZoom * 100));
    onZoomChange(newZoom);
  };

  const handleMouseDown = () => {
    isDraggingSlider.current = true;
    setIsZoomActive(true);
  };

  const handleMouseUp = () => {
    isDraggingSlider.current = false;
    setIsZoomActive(false);
    if (isZoomReady) {
      setLatchedDisplayPercent(Math.round(currentOriginalPercent * 100));
    }
  };

  const handleZoomKeyDown = (e: React.KeyboardEvent) => {
    if ((e.ctrlKey || e.metaKey) && ['z', 'y'].includes(e.key.toLowerCase())) {
      (e.target as HTMLElement).blur();
      return;
    }
    if (GLOBAL_KEYS.includes(e.key)) {
      (e.target as HTMLElement).blur();
    }
  };

  const handleResetZoom = () => {
    onZoomChange(0, true);
  };

  const handlePercentClick = () => {
    if (!isZoomReady) return;
    setIsEditingPercent(true);
    setPercentInputValue(latchedDisplayPercent.toString());
    setTimeout(() => {
      percentInputRef.current?.focus();
      percentInputRef.current?.select();
    }, 0);
  };

  const handlePercentSubmit = () => {
    const value = parseFloat(percentInputValue);
    if (!isNaN(value)) {
      const originalPercent = value / 100;
      const clampedPercent = Math.max(0.1, Math.min(2.0, originalPercent));
      onZoomChange(clampedPercent);
    }
    setIsEditingPercent(false);
    setPercentInputValue('');
  };

  const handlePercentKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter') handlePercentSubmit();
    else if (e.key === 'Escape') {
      setIsEditingPercent(false);
      setPercentInputValue('');
    }
    e.stopPropagation();
  };

  return (
    <div className="shrink-0 bg-bg-secondary rounded-lg flex flex-col">
      {!isLibraryView && showFilmstrip && (
        <div
          className={clsx(
            'overflow-hidden shrink-0 relative',
            shouldAnimate && 'transition-all duration-300 ease-in-out',
          )}
          style={{ height: `${effectiveHeight}px` }}
        >
          <div
            className={clsx(
              'w-full p-2 transition-opacity duration-300 ease-in-out',
              isCollapsed ? 'opacity-0 pointer-events-none' : 'opacity-100 pointer-events-auto',
            )}
            style={{ height: `${currentHeight}px` }}
          >
            <Filmstrip
              imageList={imageList}
              imageRatings={imageRatings}
              isLoading={isLoading}
              multiSelectedPaths={multiSelectedPaths}
              onClearSelection={onClearSelection}
              onContextMenu={onContextMenu}
              onEmptyAreaContextMenu={onEmptyAreaContextMenu}
              onImageSelect={onImageSelect}
              onRequestThumbnails={onRequestThumbnails}
              selectedImage={selectedImage}
              stackInfo={stackInfo}
              thumbnailAspectRatio={thumbnailAspectRatio}
            />
          </div>
        </div>
      )}

      <div
        className={clsx(
          'shrink-0 h-12 flex items-center justify-between px-3 relative',
          !isLibraryView && 'border-t transition-colors duration-300',
          !isLibraryView && showFilmstrip && isFilmstripVisible ? 'border-surface' : 'border-transparent',
        )}
      >
        {/* Out of the flow on purpose: the selection counter to the left grows
            and shrinks, and neither side should shift when work starts. */}
        <div className="absolute left-1/2 -translate-x-1/2 flex justify-center pointer-events-none">
          <BusyIndicator />
        </div>

        <div className="flex items-center gap-4">
          {/* BLITZRAW: the five-star rating widget used to open this bar. It is
              gone. Rating is done with the number keys and read off the tile,
              so a second place to click it earned nothing and took the corner
              the eye lands on first. */}
          <div className="flex items-center gap-2">
            <button
              className="relative w-8 h-8 flex items-center justify-center rounded-md text-text-secondary hover:bg-surface hover:text-text-primary transition-colors disabled:opacity-40 disabled:hover:bg-transparent disabled:cursor-not-allowed"
              disabled={isCopyDisabled}
              onClick={onCopy}
              data-tooltip={t('ui.bottomBar.tooltips.copySettings')}
            >
              <AnimatePresence mode="wait" initial={false}>
                {isCopied ? (
                  <motion.div
                    key="copied"
                    initial={{ opacity: 0, scale: 0.5 }}
                    animate={{ opacity: 1, scale: 1 }}
                    exit={{ opacity: 0, scale: 0.5 }}
                    transition={{ duration: 0.15 }}
                    className="absolute"
                  >
                    <Check size={18} className="text-green-500" />
                  </motion.div>
                ) : (
                  <motion.div
                    key="copy"
                    initial={{ opacity: 0, scale: 0.5 }}
                    animate={{ opacity: 1, scale: 1 }}
                    exit={{ opacity: 0, scale: 0.5 }}
                    transition={{ duration: 0.15 }}
                    className="absolute"
                  >
                    <Copy size={18} />
                  </motion.div>
                )}
              </AnimatePresence>
            </button>

            <button
              className="relative w-8 h-8 flex items-center justify-center rounded-md text-text-secondary hover:bg-surface hover:text-text-primary transition-colors disabled:opacity-40 disabled:hover:bg-transparent disabled:cursor-not-allowed"
              disabled={isPasteDisabled}
              onClick={onPaste}
              data-tooltip={t('ui.bottomBar.tooltips.pasteSettings')}
            >
              <AnimatePresence mode="wait" initial={false}>
                {isPasted ? (
                  <motion.div
                    key="pasted"
                    initial={{ opacity: 0, scale: 0.5 }}
                    animate={{ opacity: 1, scale: 1 }}
                    exit={{ opacity: 0, scale: 0.5 }}
                    transition={{ duration: 0.15 }}
                    className="absolute"
                  >
                    <Check size={18} className="text-green-500" />
                  </motion.div>
                ) : (
                  <motion.div
                    key="paste"
                    initial={{ opacity: 0, scale: 0.5 }}
                    animate={{ opacity: 1, scale: 1 }}
                    exit={{ opacity: 0, scale: 0.5 }}
                    transition={{ duration: 0.15 }}
                    className="absolute"
                  >
                    <ClipboardPaste size={18} />
                  </motion.div>
                )}
              </AnimatePresence>
            </button>

            <button
              className="w-8 h-8 flex items-center justify-center rounded-md text-text-secondary hover:bg-surface hover:text-text-primary transition-colors"
              onClick={onOpenCopyPasteSettings}
              data-tooltip={t('ui.bottomBar.tooltips.copyPasteSettings')}
            >
              <Settings size={18} />
            </button>
          </div>

          <div className="h-5 w-px bg-surface"></div>

          <div
            className={clsx(
              'flex items-center transition-all duration-300',
              isFilterExpanded ? 'bg-surface rounded-md' : 'bg-transparent',
            )}
          >
            <button
              className={clsx(
                'relative w-8 h-8 flex items-center justify-center rounded-md transition-colors shrink-0',
                isFilterExpanded ? 'text-text-primary' : 'text-text-secondary hover:bg-surface hover:text-text-primary',
              )}
              onClick={() => setIsFilterExpanded(!isFilterExpanded)}
              data-tooltip={t('ui.bottomBar.tooltips.quickFilter', 'Quick Filter')}
            >
              <Filter size={18} />
            </button>

            <div
              className={clsx(
                'flex items-center transition-all duration-300 ease-in-out overflow-hidden',
                isFilterExpanded ? 'max-w-100 opacity-100 pr-2 ml-1' : 'max-w-0 opacity-0 pr-0 ml-0',
              )}
            >
              <div className="flex items-center gap-3 whitespace-nowrap">
                <div className="flex items-center gap-0.5">
                  {[1, 2, 3, 4, 5].map((starValue) => {
                    const isFilled = filterCriteria.rating > 0 && starValue <= filterCriteria.rating;
                    return (
                      <button
                        key={`qf-star-${starValue}`}
                        onClick={() =>
                          setFilterCriteria((prev) => ({
                            ...prev,
                            rating: prev.rating === starValue ? 0 : starValue,
                          }))
                        }
                        className="p-0.5 focus:outline-none"
                      >
                        <Star
                          size={16}
                          className={clsx(
                            'transition-colors duration-150',
                            isFilled ? 'text-accent fill-accent' : 'text-text-secondary hover:text-accent',
                          )}
                        />
                      </button>
                    );
                  })}
                </div>

                <div className="h-4 w-px bg-border-color"></div>

                <div className="flex items-center gap-1.5">
                  {allColors.map((color) => {
                    const isSelected = (filterCriteria.colors || []).includes(color.name);

                    const tooltipTitle =
                      color.name === 'none'
                        ? t('library.header.viewOptions.noLabel')
                        : t(`contextMenus.colors.${color.name}`, {
                            defaultValue: color.name.charAt(0).toUpperCase() + color.name.slice(1),
                          });

                    return (
                      <button
                        key={`qf-color-${color.name}`}
                        onClick={() => {
                          const currentColors = filterCriteria.colors || [];
                          const newColors = currentColors.includes(color.name)
                            ? currentColors.filter((c) => c !== color.name)
                            : [...currentColors, color.name];
                          setFilterCriteria((prev) => ({ ...prev, colors: newColors }));
                        }}
                        className={clsx(
                          'w-4 h-4 rounded-full transition-transform hover:scale-105 flex items-center justify-center focus:outline-none',
                          isSelected ? 'ring-2 ring-accent ring-offset-1 ring-offset-bg-primary' : '',
                        )}
                        style={{ backgroundColor: color.color }}
                        data-tooltip={tooltipTitle}
                      >
                        {isSelected && <Check size={10} className="text-white drop-shadow-md" />}
                      </button>
                    );
                  })}
                </div>
              </div>
            </div>
          </div>

          {stackIds.length > 0 && (
            <>
              <div className="h-5 w-px bg-surface"></div>
              <button
                className={clsx(
                  'w-8 h-8 flex items-center justify-center rounded-md transition-colors shrink-0',
                  anyStackClosed
                    ? 'text-text-secondary hover:bg-surface hover:text-text-primary'
                    : 'text-text-primary bg-surface',
                )}
                onClick={() => toggleAllStacks(stackIds)}
                data-tooltip={
                  anyStackClosed
                    ? t('ui.bottomBar.tooltips.openAllStacks', { count: stackIds.length })
                    : t('ui.bottomBar.tooltips.closeAllStacks', { count: stackIds.length })
                }
              >
                <Layers size={18} />
              </button>
            </>
          )}

          {/* BLITZRAW: the bar always says something about the card.
              It used to collapse to nothing below two selected, so with one
              photo picked, or none, the corner went blank and the size of the
              folder was nowhere on screen. Now the selection reads when there
              is one to read, and the count of what is on screen otherwise. */}
          <div
            className={clsx(
              'flex items-center transition-all duration-300 ease-out overflow-hidden',
              // Wide enough that the long form never clips. The cap is here to
              // animate against, not to constrain: 'Selected: 159 files (136 in
              // 34 stacks, 23 individual)' ran past 20rem and lost its tail.
              'max-w-2xl opacity-100',
            )}
          >
            <div className="h-5 w-px bg-surface mr-4"></div>
            <Text as="span" className="whitespace-nowrap">
              {showSelectionCounter
                ? selectionSummary.stacks === 0
                  ? t('ui.bottomBar.imagesSelected', { current: numSelected, total })
                  : selectionSummary.individual === 0
                    ? t('ui.bottomBar.imagesSelectedInStacks', {
                        count: selectionSummary.stacks,
                        files: selectionSummary.files,
                      })
                    : t('ui.bottomBar.imagesSelectedWithStacks', {
                        clicked: selectionSummary.clicked,
                        count: selectionSummary.stacks,
                        files: selectionSummary.files,
                        individual: selectionSummary.individual,
                      })
                : isFiltered
                  ? t('ui.bottomBar.imagesShownFiltered', { shown, total })
                  : t('ui.bottomBar.imagesShown', { count: total })}
            </Text>
          </div>
        </div>

        <div className="grow" />

        <div className="flex items-center gap-4">
          {!isLibraryView && showZoomControls && (
            <>
              <div className="flex items-center gap-2 w-56">
                <div
                  className="relative w-12 h-full flex items-center justify-end cursor-pointer"
                  onClick={handleResetZoom}
                  onMouseEnter={() => setIsZoomLabelHovered(true)}
                  onMouseLeave={() => setIsZoomLabelHovered(false)}
                  data-tooltip={t('ui.bottomBar.tooltips.resetZoom')}
                >
                  <span className="absolute right-0 text-xs text-text-secondary select-none text-right w-max transition-colors hover:text-text-primary">
                    {isZoomLabelHovered ? t('ui.bottomBar.zoomLabelReset') : t('ui.bottomBar.zoomLabel')}
                  </span>
                </div>

                <div className="relative flex-1 h-5">
                  <div className="absolute top-1/2 left-0 w-full h-1.5 -translate-y-1/2 bg-surface rounded-full pointer-events-none" />
                  <input
                    type="range"
                    min={0.1}
                    max={2.0}
                    step="0.05"
                    value={latchedSliderValue}
                    onChange={handleSliderChange}
                    onKeyDown={handleZoomKeyDown}
                    onMouseDown={handleMouseDown}
                    onMouseUp={handleMouseUp}
                    onTouchStart={handleMouseDown}
                    onTouchEnd={handleMouseUp}
                    onDoubleClick={handleResetZoom}
                    className={`absolute top-1/2 left-0 w-full h-1.5 mt-[-1.5px] appearance-none bg-transparent cursor-pointer p-0 slider-input z-10 ${
                      isZoomActive ? 'slider-thumb-active' : ''
                    }`}
                  />
                </div>

                <div className="relative text-xs text-text-secondary w-6 text-right flex items-center justify-end h-5 gap-1">
                  {isEditingPercent ? (
                    <input
                      ref={percentInputRef}
                      type="text"
                      value={percentInputValue}
                      onChange={(e) => setPercentInputValue(e.target.value)}
                      onKeyDown={handlePercentKeyDown}
                      onBlur={handlePercentSubmit}
                      className="w-full text-xs text-text-primary bg-bg-primary border border-border-color rounded-sm px-1 text-right"
                      style={{ fontSize: '12px', height: '18px' }}
                    />
                  ) : (
                    <span
                      onClick={handlePercentClick}
                      className="cursor-pointer hover:text-text-primary transition-colors select-none"
                      data-tooltip={t('ui.bottomBar.tooltips.customZoom')}
                    >
                      {latchedDisplayPercent}%
                    </span>
                  )}
                </div>
              </div>

              <div className="h-5 w-px bg-surface"></div>
            </>
          )}

          <div className="flex items-center gap-1">
            {!isAndroid && (
              <>
                <PanelToggleButton
                  onClick={toggleLeft}
                  Icon={PanelLeft}
                  tooltip={isLeftOpen ? t('ui.bottomBar.tooltips.collapseLeft') : t('ui.bottomBar.tooltips.expandLeft')}
                />

                {showFilmstrip && (
                  <PanelToggleButton
                    onClick={toggleBottom}
                    Icon={PanelBottom}
                    tooltip={
                      isBottomOpen
                        ? t('ui.bottomBar.tooltips.collapseFilmstrip')
                        : t('ui.bottomBar.tooltips.expandFilmstrip')
                    }
                    disabled={isLibraryView}
                  />
                )}

                <PanelToggleButton
                  onClick={toggleRight}
                  Icon={PanelRight}
                  tooltip={
                    isRightOpen ? t('ui.bottomBar.tooltips.collapseRight') : t('ui.bottomBar.tooltips.expandRight')
                  }
                />
              </>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
