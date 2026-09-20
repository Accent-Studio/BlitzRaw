import React, { useState, useEffect, useRef, useCallback, useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { Keyboard } from 'lucide-react';
import { toast } from 'react-toastify';
import { useEditorStore } from '../../store/useEditorStore';
import { useSettingsStore } from '../../store/useSettingsStore';
import { useContextMenu } from '../../context/ContextMenuContext';
import { BUILT_IN_QUICK_ADJUSTMENTS } from '../../utils/quickAdjustments';
import { dismissWbPicker } from '../../utils/wbPicker';
import { GLOBAL_KEYS } from './AppProperties';

type SliderChangeEvent =
  | React.ChangeEvent<HTMLInputElement>
  | {
      target: {
        value: number | string;
      };
    };

interface SliderProps {
  defaultValue?: number;
  disabled?: boolean;
  label: React.ReactNode;
  max: number;
  min: number;
  onChange(event: SliderChangeEvent): void;
  onDragStateChange?(state: boolean): void;
  step: number;
  value: number;
  trackClassName?: string;
  fillOrigin?: 'min' | 'default';
  suffix?: string;
  /**
   * BLITZRAW: bounds for a typed value, when the useful part of the range is
   * narrower than the legal one. Kelvin is the case: nearly every real scene
   * sits between 2000 and 7000, and a track spanning to 50000 makes that part
   * unusably cramped, but a number outside it is still valid and has to be
   * accepted. Defaults to the track bounds, so every other slider is unchanged.
   */
  inputMin?: number;
  inputMax?: number;
  /**
   * BLITZRAW: the adjustment this slider drives, dotted where it is nested.
   * A slider that names itself can be right-clicked and bound to a key; one
   * that does not is left alone, so this stays opt-in rather than something
   * every call site has to be updated for at once.
   */
  adjustmentKey?: string;
}

/**
 * BLITZRAW: how tall one slider is, and why.
 *
 * The adjustments panel is a column of these, and the colour mixer alone is
 * twenty-four in one accordion, so every pixel of slider is paid for dozens of
 * times on one screen. Each row was 52px and is now 36px, which is a third off
 * the whole panel.
 *
 * | | was | now |
 * |---|---|---|
 * | label row to track | 4px gap | none, the label sits on the row above it |
 * | track row height | 20px | 12px |
 * | groove | 6px | 4px |
 * | thumb | 16px | 12px, in styles.css |
 * | gap to the next slider | 8px | 4px |
 *
 * **The hit area is deliberately untouched.** It is the range input, which is
 * 28px tall and the full width of the row, absolutely positioned and centred on
 * the groove; it overhangs the 12px row by 8px each way and none of the numbers
 * above change it. Rows are 36px apart and the hit areas are 28px, so two of
 * them still cannot overlap and a click still lands on the slider it is over.
 *
 * What the 8px of overhang *did* cost was the row above it: the label and the
 * number both sit in a 20px line box, and the input's top 8px covered the
 * bottom of both. Clicking the number to type a value dragged the thumb to the
 * far right. The label row is now z-20 against the input's z-10, so those two
 * small boxes win where they overlap and the hit area keeps its full height
 * everywhere else.
 */
const DOUBLE_CLICK_THRESHOLD_MS = 150;
const FINE_ADJUSTMENT_MULTIPLIER = 0.2;
const TOUCH_DRAG_THRESHOLD_PX = 10;
const TOUCH_THUMB_HIT_RADIUS_PX = 24;

const hasFineAdjustmentModifier = (event: MouseEvent | TouchEvent | React.MouseEvent | React.TouchEvent) =>
  'shiftKey' in event && (event.shiftKey || event.altKey);

const Slider = ({
  defaultValue = 0,
  disabled = false,
  label,
  max,
  min,
  onChange,
  onDragStateChange = () => {},
  step = 1,
  value,
  trackClassName,
  fillOrigin = 'default',
  suffix = '',
  inputMin,
  inputMax,
  adjustmentKey,
}: SliderProps) => {
  const { t } = useTranslation();
  const { showContextMenu } = useContextMenu();
  const [displayValue, setDisplayValue] = useState<number>(value);
  const [isDragging, setIsDragging] = useState(false);
  const animationFrameRef = useRef<number | undefined>(undefined);
  const [isEditing, setIsEditing] = useState(false);
  const [inputValue, setInputValue] = useState<string>(String(value));
  const inputRef = useRef<HTMLInputElement | null>(null);
  const rangeInputRef = useRef<HTMLInputElement | null>(null);
  const [isLabelHovered, setIsLabelHovered] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);
  const lastUpTime = useRef(0);
  const lastPointerXRef = useRef<number>(0);
  const accumulatedValueRef = useRef<number>(0);
  const pendingTouchRef = useRef<{
    startX: number;
    startY: number;
    latestX: number;
    startValue: number;
  } | null>(null);
  const suppressTouchChangeRef = useRef(false);
  const isWheelActivelyChangingRef = useRef(false);
  const wheelTimeoutRef = useRef<number | undefined>(undefined);

  useEffect(() => {
    return () => {
      if (wheelTimeoutRef.current !== undefined) {
        window.clearTimeout(wheelTimeoutRef.current);
      }
    };
  }, []);

  // BLITZRAW: clamped, because a typed value may sit outside the track and an
  // unclamped percentage would draw the fill past the end of the groove.
  const fillPercentage =
    max !== min ? Math.max(0, Math.min(100, ((displayValue - min) / (max - min)) * 100)) : 0;
  const originPercentage = useMemo(() => {
    if (fillOrigin === 'min') {
      return 0;
    }
    return max !== min ? ((defaultValue - min) / (max - min)) * 100 : 0;
  }, [fillOrigin, defaultValue, min, max]);

  const stepStr = String(step);
  const decimalPlaces = stepStr.includes('.') ? stepStr.split('.')[1].length : 0;

  const snapToStep = useCallback(
    (val: number): number => {
      const snapped = Math.round((val - min) / step) * step + min;
      const clamped = Math.max(min, Math.min(max, snapped));
      return parseFloat(clamped.toFixed(decimalPlaces));
    },
    [min, max, step, decimalPlaces],
  );

  const onChangeRef = useRef(onChange);
  const snapToStepRef = useRef(snapToStep);
  const rangeRef = useRef({ min, max });

  onChangeRef.current = onChange;
  snapToStepRef.current = snapToStep;
  rangeRef.current = { min, max };

  useEffect(() => {
    onDragStateChange(isDragging);
  }, [isDragging, onDragStateChange]);

  useEffect(() => {
    if (!disabled) return;

    pendingTouchRef.current = null;
    suppressTouchChangeRef.current = false;
    isWheelActivelyChangingRef.current = false;

    if (wheelTimeoutRef.current !== undefined) {
      window.clearTimeout(wheelTimeoutRef.current);
      wheelTimeoutRef.current = undefined;
    }
    if (animationFrameRef.current !== undefined) {
      cancelAnimationFrame(animationFrameRef.current);
      animationFrameRef.current = undefined;
    }

    setIsDragging(false);
    setIsEditing(false);
    setIsLabelHovered(false);
    setDisplayValue(value);
    setInputValue(String(value));
  }, [disabled, value]);

  useEffect(() => {
    const sliderElement = containerRef.current;
    if (!sliderElement) return;

    const handleWheel = (event: WheelEvent) => {
      if (disabled || !event.shiftKey) {
        return;
      }

      event.preventDefault();
      const direction = -Math.sign(event.deltaY || event.deltaX);
      const newValue = value + direction * step;
      const roundedNewValue = parseFloat(newValue.toFixed(decimalPlaces));

      const clampedValue = Math.max(min, Math.min(max, roundedNewValue));

      if (clampedValue !== value && !isNaN(clampedValue)) {
        isWheelActivelyChangingRef.current = true;
        setDisplayValue(clampedValue);

        if (wheelTimeoutRef.current !== undefined) {
          window.clearTimeout(wheelTimeoutRef.current);
        }
        wheelTimeoutRef.current = window.setTimeout(() => {
          isWheelActivelyChangingRef.current = false;
        }, 150);

        const syntheticEvent = {
          target: {
            value: clampedValue,
          },
        };
        onChange(syntheticEvent);
      }
    };

    sliderElement.addEventListener('wheel', handleWheel, { passive: false });

    return () => {
      sliderElement.removeEventListener('wheel', handleWheel);
    };
  }, [disabled, value, min, max, step, onChange, decimalPlaces]);

  // Handle Dragging
  useEffect(() => {
    if (!isDragging || disabled) return;

    const inputEl = rangeInputRef.current;
    if (!inputEl) return;
    const sliderWidth = inputEl.getBoundingClientRect().width || 1;

    const handlePointerMove = (e: MouseEvent | TouchEvent) => {
      let clientX: number;
      let shiftKey: boolean;

      if ('touches' in e) {
        if (e.touches.length === 0) return;
        clientX = e.touches[0].clientX;
        shiftKey = hasFineAdjustmentModifier(e);
        if (e.cancelable) e.preventDefault();
      } else {
        clientX = (e as MouseEvent).clientX;
        shiftKey = hasFineAdjustmentModifier(e);
      }

      const deltaX = clientX - lastPointerXRef.current;
      const { min: curMin, max: curMax } = rangeRef.current;

      const multiplier = shiftKey ? FINE_ADJUSTMENT_MULTIPLIER : 1;
      const deltaValue = (deltaX / sliderWidth) * (curMax - curMin) * multiplier;

      const prevAccumulated = accumulatedValueRef.current;
      accumulatedValueRef.current = Math.max(curMin, Math.min(curMax, prevAccumulated + deltaValue));

      const actualDeltaValue = accumulatedValueRef.current - prevAccumulated;
      if (deltaValue !== 0) {
        lastPointerXRef.current += deltaX * (actualDeltaValue / deltaValue);
      } else {
        lastPointerXRef.current = clientX;
      }

      const snappedValue = snapToStepRef.current(accumulatedValueRef.current);

      setDisplayValue(snappedValue);
      onChangeRef.current({ target: { value: snappedValue } });
    };

    const handlePointerUp = () => {
      lastUpTime.current = Date.now();
      pendingTouchRef.current = null;
      suppressTouchChangeRef.current = false;
      setIsDragging(false);
    };

    window.addEventListener('mousemove', handlePointerMove, { passive: false });
    window.addEventListener('mouseup', handlePointerUp);
    window.addEventListener('touchmove', handlePointerMove, { passive: false });
    window.addEventListener('touchend', handlePointerUp);
    window.addEventListener('touchcancel', handlePointerUp);

    return () => {
      window.removeEventListener('mousemove', handlePointerMove);
      window.removeEventListener('mouseup', handlePointerUp);
      window.removeEventListener('touchmove', handlePointerMove);
      window.removeEventListener('touchend', handlePointerUp);
      window.removeEventListener('touchcancel', handlePointerUp);
    };
  }, [disabled, isDragging]);

  useEffect(() => {
    if (isDragging) {
      if (animationFrameRef.current) {
        cancelAnimationFrame(animationFrameRef.current);
      }
      return;
    }

    if (isWheelActivelyChangingRef.current) {
      if (animationFrameRef.current) {
        cancelAnimationFrame(animationFrameRef.current);
      }
      setDisplayValue(value);
      return;
    }

    const startValue = displayValue;
    const endValue = value;
    const duration = 300;
    let startTime: number | null = null;

    const easeInOut = (t: number) => t * t * (3 - 2 * t);

    const animate = (timestamp: number) => {
      if (!startTime) {
        startTime = timestamp;
      }

      const progress = timestamp - startTime;
      const linearFraction = Math.min(progress / duration, 1);
      const easedFraction = easeInOut(linearFraction);
      const currentValue = startValue + (endValue - startValue) * easedFraction;
      setDisplayValue(currentValue);

      if (linearFraction < 1) {
        animationFrameRef.current = requestAnimationFrame(animate);
      }
    };

    animationFrameRef.current = requestAnimationFrame(animate);

    return () => {
      if (animationFrameRef.current) {
        cancelAnimationFrame(animationFrameRef.current);
      }
    };
  }, [value, isDragging]);

  useEffect(() => {
    if (!isEditing) {
      setInputValue(String(value));
    }
  }, [value, isEditing]);

  useEffect(() => {
    if (isEditing && inputRef.current) {
      inputRef.current?.focus();
      inputRef.current?.select();
    }
  }, [isEditing]);

  const handleReset = () => {
    if (disabled) return;
    dismissWbPicker();

    const syntheticEvent = {
      target: {
        value: defaultValue,
      },
    };
    onChange(syntheticEvent);
  };

  const handleChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    if (disabled || suppressTouchChangeRef.current) {
      return;
    }

    if (!isDragging) {
      setDisplayValue(Number(e.target.value));
      onChange(e);
    }
  };

  const handleMouseDown = (e: React.MouseEvent<HTMLInputElement>) => {
    if (disabled) return;
    // BLITZRAW: moving on from the eyedropper turns it off. See wbPicker.ts.
    dismissWbPicker();

    if (Date.now() - lastUpTime.current < DOUBLE_CLICK_THRESHOLD_MS) {
      e.preventDefault();
      return;
    }
    e.preventDefault();

    const rect = e.currentTarget.getBoundingClientRect();
    const fraction = Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width));
    const rawValue = min + fraction * (max - min);
    const snappedValue = snapToStep(rawValue);

    accumulatedValueRef.current = rawValue;
    lastPointerXRef.current = e.clientX;

    setIsDragging(true);
    setDisplayValue(snappedValue);
    onChange({ target: { value: snappedValue } });
  };

  const handleTouchStart = (e: React.TouchEvent<HTMLInputElement>) => {
    if (disabled) return;
    dismissWbPicker();

    if (e.touches.length === 0) return;

    const touch = e.touches[0];
    suppressTouchChangeRef.current = true;

    const inputEl = rangeInputRef.current;
    if (!inputEl) return;

    const rect = inputEl.getBoundingClientRect();
    const fraction = max !== min ? (displayValue - min) / (max - min) : 0;
    const thumbX = rect.left + Math.max(0, Math.min(1, fraction)) * rect.width;

    if (Math.abs(touch.clientX - thumbX) > TOUCH_THUMB_HIT_RADIUS_PX) {
      pendingTouchRef.current = null;
      return;
    }

    pendingTouchRef.current = {
      startX: touch.clientX,
      startY: touch.clientY,
      latestX: touch.clientX,
      startValue: displayValue,
    };
  };

  const handleTouchMove = (e: React.TouchEvent<HTMLInputElement>) => {
    if (disabled) return;

    if (isDragging || !pendingTouchRef.current || e.touches.length === 0) return;

    const touch = e.touches[0];
    const pendingTouch = pendingTouchRef.current;
    pendingTouch.latestX = touch.clientX;

    const deltaX = touch.clientX - pendingTouch.startX;
    const deltaY = touch.clientY - pendingTouch.startY;

    if (Math.abs(deltaY) > TOUCH_DRAG_THRESHOLD_PX && Math.abs(deltaY) > Math.abs(deltaX)) {
      pendingTouchRef.current = null;
      return;
    }

    if (Math.abs(deltaX) < TOUCH_DRAG_THRESHOLD_PX || Math.abs(deltaX) < Math.abs(deltaY)) {
      return;
    }

    const inputEl = rangeInputRef.current;
    if (!inputEl) return;

    const rect = inputEl.getBoundingClientRect();
    const multiplier = hasFineAdjustmentModifier(e) ? FINE_ADJUSTMENT_MULTIPLIER : 1;
    const rawValue = pendingTouch.startValue + (deltaX / rect.width) * (max - min) * multiplier;
    const snappedValue = snapToStep(rawValue);

    accumulatedValueRef.current = rawValue;
    lastPointerXRef.current = touch.clientX;
    pendingTouchRef.current = null;

    if (e.cancelable) {
      e.preventDefault();
    }

    setIsDragging(true);
    setDisplayValue(snappedValue);
    onChange({ target: { value: snappedValue } });
  };

  const handleTouchEnd = () => {
    pendingTouchRef.current = null;
    suppressTouchChangeRef.current = false;
  };

  const handleValueClick = () => {
    if (disabled) return;
    dismissWbPicker();

    setIsEditing(true);
  };

  // BLITZRAW: while a field is open, anything that fans an edit out across a
  // selection holds off, the same way it already holds off during a drag.
  // Previewing each character on the open image is useful; writing and
  // re-rendering every selected photo for each of them is not, and it left a
  // set of them sitting at 2000K for ten seconds. Cleared by the cleanup, so
  // closing the field, unmounting or the panel changing all release it.
  useEffect(() => {
    if (!isEditing) {
      return;
    }
    useEditorStore.getState().setEditor({ isSliderTyping: true });
    return () => {
      useEditorStore.getState().setEditor({ isSliderTyping: false });
    };
  }, [isEditing]);

  const handleInputChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    if (disabled) return;

    const textVal = e.target.value;
    if (!/^[0-9.,-]*$/.test(textVal)) {
      return;
    }
    setInputValue(textVal);
    const parseableText = textVal.replace(',', '.');
    const parsedValue = parseFloat(parseableText);

    // BLITZRAW: only while the number typed so far is one this field could
    // settle on. Half a number usually is not: 4800 passes through 4, 48 and
    // 480 on its way, and clamping those to the bottom of the track is what
    // turned a whole selection blue at 2000K before the fourth digit arrived.
    // Out of range now means keep showing the last real value and wait.
    //
    // Bounded the way the commit is, by the typed limits where they are wider
    // than the track, so what appears while typing is what will be kept.
    const lowerLimit = inputMin ?? min;
    const upperLimit = inputMax ?? max;
    if (!isNaN(parsedValue) && parsedValue >= lowerLimit && parsedValue <= upperLimit) {
      onChange({
        target: {
          value: parsedValue,
        },
      });
    }
  };

  const handleInputCommit = () => {
    if (disabled) {
      setInputValue(String(value));
      setIsEditing(false);
      return;
    }

    let newValue = parseFloat(inputValue.replace(',', '.'));
    if (isNaN(newValue)) {
      newValue = value;
    } else {
      // BLITZRAW: the typed bounds, which may be wider than the track.
      newValue = Math.max(inputMin ?? min, Math.min(inputMax ?? max, newValue));
    }
    const syntheticEvent = {
      target: {
        value: newValue,
      },
    };
    onChange(syntheticEvent);
    setIsEditing(false);
  };

  // ============ BLITZRAW: Tab between the number fields ============
  // A slider's number is a span until it is clicked, so there is no chain of
  // inputs for the browser to Tab along: it would leave the panel entirely.
  // The sliders themselves are the chain, in the order they are laid out, so
  // the move is "close this one, open the next one".
  //
  // Found through the document rather than through React, because the sliders
  // are spread across five adjustment panels with no common parent, and a
  // collapsed section renders none at all, which is exactly right: a field you
  // cannot see is not one to Tab into. Each window has its own document, so a
  // floating panel Tabs within itself.
  const openAdjacentField = (direction: 1 | -1) => {
    const container = containerRef.current;
    if (!container) {
      return;
    }

    const owner = container.ownerDocument;
    const all = Array.from(owner.querySelectorAll<HTMLElement>('[data-slider-container]'));
    const here = all.indexOf(container);
    if (here === -1) {
      return;
    }

    // Walk rather than step, so a disabled slider is passed over instead of
    // ending the chain. Its span carries no marker, which is what says so.
    for (let at = here + direction; at >= 0 && at < all.length; at += direction) {
      const field = all[at].querySelector<HTMLElement>('[data-slider-field]');
      if (field) {
        field.click();
        all[at].scrollIntoView({ block: 'nearest' });
        return;
      }
    }
  };
  // ========== BLITZRAW END: Tab between the number fields ==========

  const handleInputKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (disabled) return;

    if (e.key === 'Tab') {
      // BLITZRAW: the commit has to happen before the next field opens, or the
      // blur that follows lands on a field that has already moved on.
      e.preventDefault();
      handleInputCommit();
      openAdjacentField(e.shiftKey ? -1 : 1);
      return;
    }

    if (e.key === 'Enter') {
      handleInputCommit();
      e.currentTarget.blur();
    } else if (e.key === 'Escape') {
      setInputValue(String(value));
      setIsEditing(false);
      e.currentTarget.blur();
    } else if (e.key === 'ArrowUp' || e.key === 'ArrowDown') {
      e.preventDefault();
      let currentNum = parseFloat(inputValue.replace(',', '.'));
      if (isNaN(currentNum)) {
        currentNum = value;
      }
      const direction = e.key === 'ArrowUp' ? 1 : -1;
      const newValue = currentNum + direction * step;
      const snappedNewValue = snapToStep(newValue);
      setInputValue(String(snappedNewValue));
      onChange({
        target: {
          value: snappedNewValue,
        },
      });
    }
  };

  const handleRangeKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.ctrlKey || e.metaKey) {
      e.currentTarget.blur();
      return;
    }
    if (GLOBAL_KEYS.includes(e.key)) {
      e.currentTarget.blur();
    }
  };

  const numericValue = isNaN(Number(value)) ? 0 : Number(value);

  // BLITZRAW: right-click to bind this slider to a key. Only offered by a
  // slider that names its adjustment, since without that there is nothing to
  // nudge. Adding it is all that happens here; which key it gets is chosen in
  // Settings, where every other binding lives.
  const handleContextMenu = (event: React.MouseEvent) => {
    if (!adjustmentKey || disabled) return;
    event.preventDefault();
    event.stopPropagation();

    const settings = useSettingsStore.getState().appSettings;
    const existing = settings?.quickAdjustments ?? [];
    const alreadyAdded =
      existing.some((item) => item.id === adjustmentKey) ||
      BUILT_IN_QUICK_ADJUSTMENTS.some((item) => item.path === adjustmentKey);
    const name = typeof label === 'string' ? label : adjustmentKey;

    showContextMenu(event.clientX, event.clientY, [
      {
        label: alreadyAdded ? t('settings.keybinds.alreadyQuick') : t('settings.keybinds.addToQuick'),
        icon: Keyboard,
        disabled: alreadyAdded,
        onClick: () => {
          if (!settings) return;
          // handleSettingsChange rather than setAppSettings: the first writes
          // to disk, the second only updates the store and the entry would be
          // gone at the next launch.
          useSettingsStore.getState().handleSettingsChange({
            ...settings,
            quickAdjustments: [
              ...existing,
              { id: adjustmentKey, path: adjustmentKey, step, min, max, label: name },
            ],
          });
          toast.success(t('settings.keybinds.addedToQuick', { name }));
        },
      },
    ]);
  };

  return (
    <div
      // BLITZRAW: a third of the height it used to take. See SLIDER_DENSITY.
      className={`mb-1 group ${disabled ? 'opacity-50 cursor-not-allowed' : ''}`}
      ref={containerRef}
      data-slider-container=""
      onContextMenu={adjustmentKey ? handleContextMenu : undefined}
    >
      <div className="flex justify-between items-center">
        <div
          // Same reason as the number field below: the label is a reset button
          // and the slider was eating the bottom of it.
          className={`relative z-20 grid ${typeof label === 'string' && !disabled ? 'cursor-pointer' : ''}`}
          onClick={typeof label === 'string' && !disabled ? handleReset : undefined}
          onDoubleClick={typeof label === 'string' && !disabled ? handleReset : undefined}
          onMouseEnter={typeof label === 'string' && !disabled ? () => setIsLabelHovered(true) : undefined}
          onMouseLeave={typeof label === 'string' && !disabled ? () => setIsLabelHovered(false) : undefined}
        >
          <span
            aria-hidden={isLabelHovered && typeof label === 'string'}
            className={`col-start-1 row-start-1 text-sm font-medium text-text-secondary select-none transition-opacity duration-200 ease-in-out ${
              isLabelHovered && typeof label === 'string' ? 'opacity-0' : 'opacity-100'
            }`}
          >
            {label}
          </span>
          {typeof label === 'string' && (
            <span
              aria-hidden={!isLabelHovered}
              className={`col-start-1 row-start-1 text-sm font-medium text-text-primary select-none transition-opacity duration-200 ease-in-out pointer-events-none ${
                isLabelHovered ? 'opacity-100' : 'opacity-0'
              }`}
            >
              {t('ui.slider.reset')}
            </span>
          )}
        </div>
        {/* BLITZRAW: the number field wins where it meets the slider.
            The range input below is 28px tall on a 12px row, so it reaches 8px
            up into this row and takes the bottom third of the number with it.
            Clicking the number to type a value dragged the thumb to the far
            right instead. z-20 puts the field above the input's z-10, so the
            48px under the number belongs to the number and the hit area is
            untouched everywhere else. */}
        <div className="relative z-20 w-12 text-right">
          {isEditing ? (
            <input
              className="w-full text-sm text-right bg-card-active border border-gray-500 rounded-sm px-1 py-0 outline-none focus:ring-1 focus:ring-blue-500 text-text-primary"
              disabled={disabled}
              max={max}
              min={min}
              onBlur={handleInputCommit}
              onChange={handleInputChange}
              onKeyDown={handleInputKeyDown}
              ref={inputRef}
              step={step}
              type="text"
              value={inputValue}
            />
          ) : (
            <span
              className={`text-sm text-text-primary w-full text-right select-none ${disabled ? '' : 'cursor-text'}`}
              data-slider-field={disabled ? undefined : ''}
              onClick={disabled ? undefined : handleValueClick}
              onDoubleClick={disabled ? undefined : handleReset}
              data-tooltip={disabled ? undefined : t('ui.slider.clickToEdit')}
            >
              {decimalPlaces > 0 && numericValue === 0 ? '0' : numericValue.toFixed(decimalPlaces)}
              {suffix && <span className="text-[10px] align-top inline-block mt-0.5 ml-0.5">{suffix}</span>}
            </span>
          )}
        </div>
      </div>

      {/* BLITZRAW: 12px of layout for a 4px groove. The range input inside is
          28px tall and centred on it, so it overhangs this row by 8px each way
          and the hit area is unchanged. Rows are 36px apart, so two of those
          hit areas still cannot touch. */}
      <div className="relative w-full h-3">
        <div
          className={`absolute top-1/2 left-0 w-full h-1 -translate-y-1/2 rounded-full pointer-events-none ${
            trackClassName || 'bg-card-active'
          }`}
        />
        <div
          className="absolute top-1/2 h-1 -translate-y-1/2 rounded-full pointer-events-none bg-accent/25"
          style={{
            left: `${Math.min(fillPercentage, originPercentage)}%`,
            width: `${Math.abs(fillPercentage - originPercentage)}%`,
          }}
        />
        <input
          ref={rangeInputRef}
          className={`absolute top-1/2 left-0 w-full h-7 -translate-y-1/2 appearance-none bg-transparent cursor-pointer m-0 p-0 slider-input z-10 ${
            isDragging ? 'slider-thumb-active' : ''
          } ${disabled ? 'cursor-not-allowed' : ''}`}
          style={{ margin: 0, touchAction: isDragging ? 'none' : 'pan-y' }}
          max={String(max)}
          min={String(min)}
          onChange={handleChange}
          onDoubleClick={handleReset}
          onKeyDown={handleRangeKeyDown}
          onMouseDown={handleMouseDown}
          onTouchStart={handleTouchStart}
          onTouchMove={handleTouchMove}
          onTouchEnd={handleTouchEnd}
          onTouchCancel={handleTouchEnd}
          step={String(step)}
          type="range"
          value={displayValue}
        />
      </div>
    </div>
  );
};

export default Slider;
