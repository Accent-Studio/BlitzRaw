import { useState, useEffect, useMemo } from 'react';
import { Pipette, Sliders } from 'lucide-react';
import { motion, AnimatePresence } from 'framer-motion';
import { useTranslation } from 'react-i18next';
import Slider from '../ui/Slider';
import ColorWheel from '../ui/ColorWheel';
import { invoke } from '@tauri-apps/api/core';
import {
  ColorAdjustment,
  ColorCalibration,
  HueSatLum,
  INITIAL_ADJUSTMENTS,
  wholeWhiteBalance,
} from '../../utils/adjustments';
import { Adjustments, ColorGrading } from '../../utils/adjustments';
import { AppSettings } from '../ui/AppProperties';
import Text from '../ui/Text';
import { TextColors, TextVariants, TextWeights } from '../../types/typography';

interface ColorPanelProps {
  adjustments: Adjustments;
  setAdjustments(adjustments: Partial<Adjustments>): any;
  appSettings: AppSettings | null;
  isForMask?: boolean;
  isWbPickerActive?: boolean;
  toggleWbPicker?: () => void;
  onDragStateChange?: (isDragging: boolean) => void;
  /**
   * BLITZRAW: needed to read the file's own as-shot white balance. `isRaw`
   * carries the layout until the backend answers, so the panel does not flip
   * between the two pairs of sliders on every image change.
   */
  selectedImage?: { path?: string; isRaw?: boolean } | null;
}

/**
 * BLITZRAW: where the Kelvin track starts and ends.
 *
 * Not the limits of what is valid, which the backend reports and which reach
 * far wider. These are the limits of what is useful to drag through: roughly
 * candlelight to open shade, which covers every event and interior we shoot.
 */
const KELVIN_TRACK_MIN = 2000;
const KELVIN_TRACK_MAX = 7000;
export const KELVIN_STEP = 50;

/** BLITZRAW: what the backend reports about a file's white balance. */
interface WhiteBalanceInfo {
  hasProfile: boolean;
  asShotKelvin: number;
  asShotTint: number;
  minKelvin: number;
  maxKelvin: number;
  minTint: number;
  maxTint: number;
  hasForwardMatrix: boolean;
}

interface ColorSwatchProps {
  color: string;
  isActive: boolean;
  name: string;
  ariaLabel: string;
  onClick: (name: string) => void;
}

const ColorSwatch = ({ color, name, isActive, ariaLabel, onClick }: ColorSwatchProps) => {
  const [isPressed, setIsPressed] = useState(false);
  const [isHovered, setIsHovered] = useState(false);

  const handleMouseDown = () => {
    setIsPressed(true);
  };

  const handleMouseUp = () => {
    setIsPressed(false);
  };

  const handleMouseLeave = () => {
    setIsPressed(false);
    setIsHovered(false);
  };

  const handleMouseEnter = () => {
    setIsHovered(true);
  };

  const handleClick = () => {
    onClick(name);
  };

  const getTransform = () => {
    if (isPressed) return 'scale(0.95)';
    if (isActive) return 'scale(1.1)';
    if (isHovered) return 'scale(1.08)';
    return 'scale(1)';
  };

  return (
    <button
      aria-label={ariaLabel}
      className="relative w-6 h-6 focus:outline-hidden group"
      onClick={handleClick}
      onMouseDown={handleMouseDown}
      onMouseUp={handleMouseUp}
      onMouseLeave={handleMouseLeave}
      onMouseEnter={handleMouseEnter}
      onTouchStart={handleMouseDown}
      onTouchEnd={handleMouseUp}
    >
      <div
        className={`absolute inset-0 rounded-full border-2 transition-all duration-200 ease-out ${
          isActive ? 'border-white opacity-100' : 'scale-100 border-transparent opacity-0'
        }`}
        style={{
          transform: isActive ? (isPressed ? 'scale(1.1)' : 'scale(1.25)') : undefined,
          transition: isPressed
            ? 'transform 100ms cubic-bezier(0.4, 0, 0.2, 1), opacity 200ms ease-out'
            : 'transform 200ms cubic-bezier(0.34, 1.56, 0.64, 1), opacity 200ms ease-out',
        }}
      />

      <div
        className={`absolute inset-0 rounded-full transition-all duration-150 ease-out ${
          isActive ? 'shadow-lg' : 'shadow-md'
        }`}
        style={{
          backgroundColor: color,
          transform: getTransform(),
          transition: isPressed
            ? 'transform 100ms cubic-bezier(0.4, 0, 0.2, 1)'
            : 'transform 200ms cubic-bezier(0.34, 1.56, 0.64, 1)',
        }}
      />
    </button>
  );
};

const ColorGradingPanel = ({ adjustments, setAdjustments, onDragStateChange }: ColorPanelProps) => {
  const { t } = useTranslation();
  const [activeTab, setActiveTab] = useState<'3way' | 'global'>('3way');
  const [isExpanded, setIsExpanded] = useState(false);
  const colorGrading = adjustments.colorGrading || INITIAL_ADJUSTMENTS.colorGrading;

  const handleChange = (grading: ColorGrading, newValue: HueSatLum) => {
    setAdjustments((prev: Partial<Adjustments>) => ({
      ...prev,
      colorGrading: {
        ...(prev.colorGrading || INITIAL_ADJUSTMENTS.colorGrading),
        [grading]: newValue,
      },
    }));
  };

  const handleColorGradingSliderChange = (grading: ColorGrading, value: string) => {
    setAdjustments((prev: Partial<Adjustments>) => ({
      ...prev,
      colorGrading: {
        ...(prev.colorGrading || INITIAL_ADJUSTMENTS.colorGrading),
        [grading]: parseFloat(value),
      },
    }));
  };

  const tabs = useMemo(
    () => [
      {
        id: '3way',
        icon: (
          <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor">
            <circle cx="12" cy="6" r="4.5" />
            <circle cx="5" cy="18" r="4.5" />
            <circle cx="19" cy="18" r="4.5" />
          </svg>
        ),
      },
      {
        id: 'global',
        icon: (
          <div className="w-3.5 h-3.5 rounded-full" style={{ background: 'linear-gradient(to top, #666, #fff)' }} />
        ),
      },
    ],
    [],
  );

  return (
    <div>
      <div className="flex items-center justify-start gap-2 mb-4 mt-2">
        {tabs.map((tab) => {
          const isActive = activeTab === tab.id;
          return (
            <button
              key={tab.id}
              onClick={() => setActiveTab(tab.id as '3way' | 'global')}
              className={`w-7 h-7 rounded-full flex items-center justify-center transition-all focus:outline-none
                ${
                  isActive
                    ? 'ring-2 ring-offset-2 ring-offset-surface ring-accent text-text-primary'
                    : 'bg-bg-secondary text-text-secondary hover:text-text-primary hover:bg-bg-secondary/80'
                }`}
            >
              {tab.icon}
            </button>
          );
        })}

        <div className="w-px h-5 bg-text-secondary/20 mx-1" />

        <button
          onClick={() => setIsExpanded(!isExpanded)}
          className={`w-7 h-7 rounded-full flex items-center justify-center transition-all focus:outline-none
            ${
              isExpanded
                ? 'bg-accent text-button-text'
                : 'bg-bg-secondary text-text-secondary hover:text-text-primary hover:bg-bg-secondary/80'
            }`}
          data-tooltip={t('adjustments.color.toggleSliders')}
        >
          <Sliders size={14} />
        </button>
      </div>

      <div className="relative w-full mb-4">
        <AnimatePresence mode="wait">
          {activeTab === '3way' ? (
            <motion.div
              key="3way"
              initial={{ opacity: 0, x: -15 }}
              animate={{ opacity: 1, x: 0 }}
              exit={{ opacity: 0, x: -15 }}
              transition={{ duration: 0.2 }}
              className="w-full"
            >
              <div className="flex justify-center mb-4">
                <div className="w-[calc(50%-0.5rem)]">
                  <ColorWheel
                    defaultValue={INITIAL_ADJUSTMENTS.colorGrading.midtones}
                    label={t('adjustments.color.grading.midtones')}
                    onChange={(val: HueSatLum) => handleChange(ColorGrading.Midtones, val)}
                    value={colorGrading.midtones}
                    onDragStateChange={onDragStateChange}
                    isExpanded={isExpanded}
                  />
                </div>
              </div>
              <div className="flex justify-between mb-2 gap-4">
                <div className="w-full flex-1 min-w-0">
                  <ColorWheel
                    defaultValue={INITIAL_ADJUSTMENTS.colorGrading.shadows}
                    label={t('adjustments.color.grading.shadows')}
                    onChange={(val: HueSatLum) => handleChange(ColorGrading.Shadows, val)}
                    value={colorGrading.shadows}
                    onDragStateChange={onDragStateChange}
                    isExpanded={isExpanded}
                  />
                </div>
                <div className="w-full flex-1 min-w-0">
                  <ColorWheel
                    defaultValue={INITIAL_ADJUSTMENTS.colorGrading.highlights}
                    label={t('adjustments.color.grading.highlights')}
                    onChange={(val: HueSatLum) => handleChange(ColorGrading.Highlights, val)}
                    value={colorGrading.highlights}
                    onDragStateChange={onDragStateChange}
                    isExpanded={isExpanded}
                  />
                </div>
              </div>
            </motion.div>
          ) : (
            <motion.div
              key="global"
              initial={{ opacity: 0, x: 15 }}
              animate={{ opacity: 1, x: 0 }}
              exit={{ opacity: 0, x: 15 }}
              transition={{ duration: 0.2 }}
              className="w-full flex justify-center pb-2"
            >
              <div className="w-full max-w-70">
                <ColorWheel
                  defaultValue={INITIAL_ADJUSTMENTS.colorGrading.global}
                  label={t('adjustments.color.grading.global')}
                  onChange={(val: HueSatLum) => handleChange(ColorGrading.Global, val)}
                  value={colorGrading.global || INITIAL_ADJUSTMENTS.colorGrading.global}
                  onDragStateChange={onDragStateChange}
                  isExpanded={isExpanded}
                />
              </div>
            </motion.div>
          )}
        </AnimatePresence>
      </div>

      <div>
        <Slider
          defaultValue={50}
          label={t('adjustments.color.grading.blending')}
          max={100}
          min={0}
          onChange={(e: any) => handleColorGradingSliderChange(ColorGrading.Blending, e.target.value)}
          step={1}
          value={colorGrading.blending}
          onDragStateChange={onDragStateChange}
        />
        <Slider
          defaultValue={0}
          label={t('adjustments.color.grading.balance')}
          max={100}
          min={-100}
          onChange={(e: any) => handleColorGradingSliderChange(ColorGrading.Balance, e.target.value)}
          step={1}
          value={colorGrading.balance}
          onDragStateChange={onDragStateChange}
        />
      </div>
    </div>
  );
};

const ColorCalibrationPanel = ({ adjustments, setAdjustments, onDragStateChange }: ColorPanelProps) => {
  const { t } = useTranslation();
  const [activePrimary, setActivePrimary] = useState('red');
  const colorCalibration = adjustments.colorCalibration || INITIAL_ADJUSTMENTS.colorCalibration;

  const PRIMARY_COLORS = useMemo(
    () => [
      { name: 'red', color: '#f87171', label: t('adjustments.color.calibration.colors.red') },
      { name: 'green', color: '#4ade80', label: t('adjustments.color.calibration.colors.green') },
      { name: 'blue', color: '#60a5fa', label: t('adjustments.color.calibration.colors.blue') },
    ],
    [t],
  );

  const handleShadowsChange = (value: string) => {
    setAdjustments((prev: Partial<Adjustments>) => ({
      ...prev,
      colorCalibration: {
        ...(prev.colorCalibration || INITIAL_ADJUSTMENTS.colorCalibration),
        shadowsTint: parseFloat(value),
      },
    }));
  };

  const handlePrimaryChange = (key: 'Hue' | 'Saturation', value: string) => {
    const fullKey = `${activePrimary}${key}` as keyof ColorCalibration;
    setAdjustments((prev: Partial<Adjustments>) => ({
      ...prev,
      colorCalibration: {
        ...(prev.colorCalibration || INITIAL_ADJUSTMENTS.colorCalibration),
        [fullKey]: parseFloat(value),
      },
    }));
  };

  const currentValues = {
    hue: colorCalibration[`${activePrimary}Hue` as keyof ColorCalibration] || 0,
    saturation: colorCalibration[`${activePrimary}Saturation` as keyof ColorCalibration] || 0,
  };

  const trackSuffix = `${activePrimary}s`;

  return (
    <div className="p-2 bg-bg-tertiary rounded-md mt-4">
      <Text variant={TextVariants.heading} className="mb-2">
        {t('adjustments.color.calibration.title')}
      </Text>
      <div>
        <Text color={TextColors.primary} weight={TextWeights.medium} className="mb-1">
          {t('adjustments.color.calibration.shadows')}
        </Text>
        <Slider
          label={t('adjustments.color.calibration.tint')}
          min={-100}
          max={100}
          step={1}
          defaultValue={0}
          value={colorCalibration.shadowsTint}
          onChange={(e: any) => handleShadowsChange(e.target.value)}
          onDragStateChange={onDragStateChange}
          trackClassName="tint-gradient-track"
        />
      </div>
      <div className="mt-3">
        <Text color={TextColors.primary} weight={TextWeights.medium} className="mb-3">
          {t('adjustments.color.calibration.primaries')}
        </Text>
        <div className="flex justify-center gap-6 mb-4 px-1">
          {PRIMARY_COLORS.map(({ name, color, label }) => (
            <ColorSwatch
              color={color}
              isActive={activePrimary === name}
              key={name}
              name={name}
              onClick={setActivePrimary}
              ariaLabel={t('adjustments.color.ariaSelectColor', { name: label })}
            />
          ))}
        </div>
        <Slider
          label={t('adjustments.color.calibration.hue')}
          min={-100}
          max={100}
          step={1}
          defaultValue={0}
          value={currentValues.hue}
          onChange={(e: any) => handlePrimaryChange('Hue', e.target.value)}
          onDragStateChange={onDragStateChange}
          trackClassName={`hue-slider-${trackSuffix}`}
        />
        <Slider
          label={t('adjustments.color.calibration.saturation')}
          min={-100}
          max={100}
          step={1}
          defaultValue={0}
          value={currentValues.saturation}
          onChange={(e: any) => handlePrimaryChange('Saturation', e.target.value)}
          onDragStateChange={onDragStateChange}
          trackClassName={`sat-slider-${trackSuffix}`}
        />
      </div>
    </div>
  );
};

// ============ BLITZRAW: colour split into three panels ============
// One accordion used to hold white balance, presence, a global hue shift, the
// grading wheels, the mixer and the calibration. Six unrelated jobs behind one
// heading, and the two reached on every single photo were buried at the top of
// a list you had to scroll past four others to leave.
//
// Split by when they are reached. Correction sits under Basic because it is
// part of getting the photo right; the mixer and the grading sit below the
// curve because they are choices made afterwards. The shared helpers above
// (the swatch, the wheels, the calibration) are untouched and used by whichever
// panel needs them.

/**
 * BLITZRAW: the eight bands the mixer divides the spectrum into, and where each
 * one sits on the wheel.
 *
 * The hue is what the coloured slider tracks are built from: each track is a
 * gradient around that band's own hue, so a saturation slider for reds is red.
 * Kept beside the names so the two can never drift apart.
 */
type MixerBand = 'reds' | 'oranges' | 'yellows' | 'greens' | 'aquas' | 'blues' | 'purples' | 'magentas';

const MIXER_BANDS: Array<{ name: MixerBand; swatch: string; hue: number }> = [
  { name: 'reds', swatch: '#f87171', hue: 0 },
  { name: 'oranges', swatch: '#fb923c', hue: 30 },
  { name: 'yellows', swatch: '#facc15', hue: 60 },
  { name: 'greens', swatch: '#4ade80', hue: 120 },
  { name: 'aquas', swatch: '#2dd4bf', hue: 180 },
  { name: 'blues', swatch: '#60a5fa', hue: 240 },
  { name: 'purples', swatch: '#a78bfa', hue: 300 },
  { name: 'magentas', swatch: '#f472b6', hue: 340 },
];

/**
 * The three things the mixer can do to a band, and their slider track names.
 *
 * `label` is spelt as a union rather than a string so the translation keys
 * built from it are literal keys that i18next can check, rather than "some
 * string appended to a prefix", which it cannot.
 */
type MixerChannel = 'hue' | 'saturation' | 'luminance';

const MIXER_CHANNELS: Array<{ key: ColorAdjustment; label: MixerChannel; track: string }> = [
  { key: ColorAdjustment.Hue, label: 'hue', track: 'hue-slider' },
  { key: ColorAdjustment.Saturation, label: 'saturation', track: 'sat-slider' },
  { key: ColorAdjustment.Luminance, label: 'luminance', track: 'lum-slider' },
];

type MixerView = 'hue' | 'saturation' | 'luminance' | 'all';

/**
 * BLITZRAW: white balance and presence. Everything a photo needs before it is a
 * photograph rather than a choice made about one.
 */
export function ColorCorrectionPanel({
  adjustments,
  setAdjustments,
  isForMask = false,
  isWbPickerActive = false,
  toggleWbPicker,
  onDragStateChange,
  selectedImage,
}: ColorPanelProps) {
  const { t } = useTranslation();

  // ============== BLITZRAW: real white balance, in Kelvin ==============
  // The camera profile lives in the file, so the panel has to ask for it. A
  // mask has no file of its own, and its temperature stays the local tint it
  // always was, the same way Lightroom's local temperature is an offset rather
  // than a Kelvin.
  const imagePath = isForMask ? undefined : selectedImage?.path;

  // Stored with the path it describes. Without that the previous image's
  // answer stays on screen while the next one is in flight, and a drag in that
  // window writes one photo's as-shot Kelvin onto another.
  const [wbState, setWbState] = useState<{ path: string; info: WhiteBalanceInfo } | null>(null);

  useEffect(() => {
    if (!imagePath) {
      setWbState(null);
      return;
    }
    let cancelled = false;
    invoke<WhiteBalanceInfo>('get_white_balance_info', { path: imagePath })
      .then((info) => {
        if (!cancelled) {
          setWbState({ path: imagePath, info });
        }
      })
      .catch(() => {
        if (!cancelled) {
          setWbState(null);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [imagePath]);

  const wbInfo = wbState && wbState.path === imagePath ? wbState.info : null;
  const isWbLoading = !!imagePath && !wbInfo;

  // While the answer is in flight, lay out from what the library already knows.
  // Guessing from isRaw rather than falling back to the legacy sliders is what
  // stops the panel flipping between the two pairs on every image change. The
  // fetch then confirms it, and only disagrees for a RAW the decoder cannot
  // read, which renders from its embedded JPEG and correctly has no Kelvin.
  const hasCameraProfile = wbInfo ? wbInfo.hasProfile : !!selectedImage?.isRaw;

  // Null means as-shot, so the slider shows what the camera chose until the
  // moment it is moved.
  const kelvin = adjustments.whiteBalance?.kelvin ?? wbInfo?.asShotKelvin ?? 5500;
  const wbTint = adjustments.whiteBalance?.tint ?? wbInfo?.asShotTint ?? 0;
  const isAsShot = !adjustments.whiteBalance;

  const setWhiteBalance = (next: { kelvin?: number; tint?: number }) =>
    setAdjustments((prev: Adjustments) => ({
      ...prev,
      // BLITZRAW: rounded, because the as-shot value comes out of the solver as
      // a float and this is where it would otherwise be written into the photo.
      // See wholeWhiteBalance.
      whiteBalance: wholeWhiteBalance({
        kelvin: next.kelvin ?? prev.whiteBalance?.kelvin ?? wbInfo?.asShotKelvin ?? 5500,
        tint: next.tint ?? prev.whiteBalance?.tint ?? wbInfo?.asShotTint ?? 0,
      }),
    }));

  const resetToAsShot = () => setAdjustments((prev: Adjustments) => ({ ...prev, whiteBalance: null }));
  // ============ BLITZRAW END: real white balance, in Kelvin ============

  const handleAdjustmentChange = (key: ColorAdjustment, value: string) => {
    setAdjustments((prev: Partial<Adjustments>) => ({ ...prev, [key]: parseFloat(value) }));
  };

  return (
    <div className="space-y-4">
      <div className="p-2 bg-bg-tertiary rounded-md">
        <div className="flex justify-between items-center mb-2">
          <Text variant={TextVariants.heading}>{t('adjustments.color.whiteBalance')}</Text>
          {!isForMask && toggleWbPicker && (
            <button
              onClick={toggleWbPicker}
              className={`p-1.5 rounded-md transition-colors ${
                isWbPickerActive ? 'bg-accent text-button-text' : 'hover:bg-bg-secondary text-text-secondary'
              }`}
              data-tooltip={t('adjustments.color.wbPickerTooltip')}
            >
              <Pipette size={16} />
            </button>
          )}
        </div>
        {/* ============== BLITZRAW: real white balance, in Kelvin ==============
            With a camera profile the sliders are the real thing, interpolated
            between the camera's own calibration illuminants. Without one there
            is nothing to anchor a Kelvin to, so the old relative tint stays:
            that is the honest control for a JPEG. */}
        {hasCameraProfile ? (
          <>
            <Slider
              label={t('adjustments.color.temperature')}
              // The track covers the range real scenes actually fall in, from
              // candlelight to open shade. Spanning the whole legal range
              // instead would squeeze every ordinary photo into the first
              // tenth of the groove. Typing still reaches the rest.
              max={KELVIN_TRACK_MAX}
              min={KELVIN_TRACK_MIN}
              inputMax={wbInfo?.maxKelvin ?? 50000}
              inputMin={wbInfo?.minKelvin ?? 1667}
              onChange={(e: any) => setWhiteBalance({ kelvin: parseFloat(e.target.value) })}
              // Fifty, as Lightroom does. Ten is finer than anyone can see and
              // makes dragging to a round number needlessly fiddly.
              step={KELVIN_STEP}
              value={kelvin}
              adjustmentKey="whiteBalance.kelvin"
              suffix="K"
              // Until the file's own as-shot white is known there is nothing
              // to move relative to, and a drag would commit a placeholder.
              disabled={isWbLoading}
              // As-shot is the zero of this slider, so it is what a
              // double-click returns to and where the fill starts.
              defaultValue={wbInfo?.asShotKelvin ?? 5500}
              fillOrigin="default"
              trackClassName="temperature-gradient-track"
              onDragStateChange={onDragStateChange}
            />
            <Slider
              label={t('adjustments.color.tint')}
              max={wbInfo?.maxTint ?? 150}
              min={wbInfo?.minTint ?? -150}
              onChange={(e: any) => setWhiteBalance({ tint: parseFloat(e.target.value) })}
              step={1}
              value={wbTint}
              disabled={isWbLoading}
              defaultValue={wbInfo?.asShotTint ?? 0}
              fillOrigin="default"
              trackClassName="tint-gradient-track"
              onDragStateChange={onDragStateChange}
            />
            <button
              onClick={resetToAsShot}
              disabled={isAsShot || isWbLoading}
              className="mt-1 w-full text-xs py-1 rounded-md transition-colors disabled:opacity-40 disabled:cursor-default hover:bg-bg-secondary text-text-secondary"
            >
              {isAsShot ? t('adjustments.color.wbAsShot') : t('adjustments.color.wbResetAsShot')}
            </button>
          </>
        ) : (
          <>
            <Slider
              label={t('adjustments.color.temperature')}
              max={100}
              min={-100}
              onChange={(e: any) => handleAdjustmentChange(ColorAdjustment.Temperature, e.target.value)}
              step={1}
              value={adjustments.temperature || 0}
              trackClassName="temperature-gradient-track"
              onDragStateChange={onDragStateChange}
            />
            <Slider
              label={t('adjustments.color.tint')}
              max={100}
              min={-100}
              onChange={(e: any) => handleAdjustmentChange(ColorAdjustment.Tint, e.target.value)}
              step={1}
              value={adjustments.tint || 0}
              trackClassName="tint-gradient-track"
              onDragStateChange={onDragStateChange}
            />
          </>
        )}
        {/* ============ BLITZRAW END: real white balance, in Kelvin ============ */}
      </div>

      <div className="p-2 bg-bg-tertiary rounded-md">
        <Text variant={TextVariants.heading} className="mb-2">
          {t('adjustments.color.presence')}
        </Text>
        <Slider
          label={t('adjustments.color.vibrance')}
          max={100}
          min={-100}
          onChange={(e: any) => handleAdjustmentChange(ColorAdjustment.Vibrance, e.target.value)}
          step={1}
          value={adjustments.vibrance || 0}
          onDragStateChange={onDragStateChange}
        />
        <Slider
          label={t('adjustments.color.saturation')}
          max={100}
          min={-100}
          onChange={(e: any) => handleAdjustmentChange(ColorAdjustment.Saturation, e.target.value)}
          step={1}
          value={adjustments.saturation || 0}
          onDragStateChange={onDragStateChange}
        />
      </div>
    </div>
  );
}

/**
 * BLITZRAW: the per-colour mixer, laid out the way Lightroom lays it out.
 *
 * It was here before, as a row of swatches and three sliders for whichever one
 * was picked. That shows one band at a time, and the whole point of a mixer is
 * comparing bands: pulling the greens down and the aquas up is one decision,
 * not two, and it cannot be seen through a control that only draws one of them.
 *
 * Four views. Three show one channel across all eight bands, which is how you
 * find a band. **All** shows the whole grid, which is how you judge a set of
 * moves together, and is what the panel opens on.
 *
 * Every track keeps the colour it describes: a saturation slider for reds runs
 * grey to red, and its hue slider moves with the band as the band is moved. The
 * CSS reads those from custom properties, and all eight are kept current rather
 * than only the selected one, because in this layout all eight are on screen.
 */
export function ColorMixerPanel({ adjustments, setAdjustments, onDragStateChange }: ColorPanelProps) {
  const { t } = useTranslation();
  const [view, setView] = useState<MixerView>('all');

  const hsl = adjustments?.hsl;

  useEffect(() => {
    for (const band of MIXER_BANDS) {
      const values = hsl?.[band.name] || { hue: 0, saturation: 0, luminance: 0 };
      const shifted = ((((band.hue + (values.hue || 0)) % 360) + 360) % 360).toString();
      // The saturation track has to show the band as it now is, and the stored
      // number is a shift from -100 to 100 rather than a saturation.
      const saturation = `${((values.saturation || 0) + 100) / 2}%`;

      document.documentElement.style.setProperty(`--hsl-mixer-hue-${band.name}`, shifted);
      document.documentElement.style.setProperty(`--hsl-mixer-sat-${band.name}`, saturation);
    }
  }, [hsl]);

  const setBand = (band: string, key: ColorAdjustment, value: string) => {
    setAdjustments((prev: Partial<Adjustments>) => ({
      ...prev,
      hsl: {
        ...(prev.hsl || {}),
        [band]: {
          ...(prev.hsl?.[band] || {}),
          [key]: parseFloat(value),
        },
      },
    }));
  };

  const channelRows = (channel: (typeof MIXER_CHANNELS)[number]) =>
    MIXER_BANDS.map((band) => (
      <Slider
        key={`${channel.label}-${band.name}`}
        label={t(`adjustments.color.mixerColors.${band.name}`)}
        max={100}
        min={-100}
        step={1}
        value={hsl?.[band.name]?.[channel.label] ?? 0}
        onChange={(e: any) => setBand(band.name, channel.key, e.target.value)}
        trackClassName={`${channel.track}-${band.name}`}
        onDragStateChange={onDragStateChange}
      />
    ));

  // All first, because it is what the panel opens on and what most work is
  // done in. The three single-channel views are for narrowing down.
  const views: Array<{ id: MixerView; label: string }> = [
    { id: 'all', label: t('adjustments.color.mixerAll') },
    { id: 'hue', label: t('adjustments.color.hue') },
    { id: 'saturation', label: t('adjustments.color.saturation') },
    { id: 'luminance', label: t('adjustments.color.luminance') },
  ];

  const shown = view === 'all' ? MIXER_CHANNELS : MIXER_CHANNELS.filter((c) => c.label === view);

  return (
    <div className="space-y-3">
      <div className="flex gap-1 p-1 bg-bg-tertiary rounded-md">
        {views.map(({ id, label }) => (
          <button
            key={id}
            onClick={() => setView(id)}
            className={`flex-1 text-xs py-1 rounded transition-colors ${
              view === id ? 'bg-surface text-text-primary' : 'text-text-secondary hover:bg-bg-secondary'
            }`}
          >
            {label}
          </button>
        ))}
      </div>

      {shown.map((channel) => (
        <div className="p-2 bg-bg-tertiary rounded-md" key={channel.label}>
          {/* Only when more than one is showing. A single channel already says
              which it is in the row of buttons above, and repeating it wastes
              a line of a panel that is eight sliders tall. */}
          {view === 'all' && (
            <Text variant={TextVariants.heading} className="mb-2">
              {t(`adjustments.color.${channel.label}`)}
            </Text>
          )}
          {channelRows(channel)}
        </div>
      ))}
    </div>
  );
}

/**
 * BLITZRAW: what is left once correction and the mixer have their own panels,
 * and what the heading now honestly describes. A global hue shift, the grading
 * wheels, and the camera calibration.
 */
export default function ColorPanel({
  adjustments,
  setAdjustments,
  appSettings,
  isForMask = false,
  onDragStateChange,
}: ColorPanelProps) {
  const { t } = useTranslation();
  const adjustmentVisibility = appSettings?.adjustmentVisibility || {};

  const handleAdjustmentChange = (key: ColorAdjustment, value: string) => {
    setAdjustments((prev: Partial<Adjustments>) => ({ ...prev, [key]: parseFloat(value) }));
  };

  return (
    <div className="space-y-4">
      <div className="p-2 bg-bg-tertiary rounded-md">
        <Text variant={TextVariants.heading} className="mb-2">
          {isForMask ? t('adjustments.color.localHue') : t('adjustments.color.hue')}
        </Text>
        <Slider
          label={t('adjustments.color.hue')}
          max={180}
          min={-180}
          onChange={(e: any) => handleAdjustmentChange(ColorAdjustment.Hue, e.target.value)}
          step={1}
          value={adjustments.hue || 0}
          trackClassName="hue-range-track"
          onDragStateChange={onDragStateChange}
        />
      </div>

      <div className="p-2 bg-bg-tertiary rounded-md">
        <Text variant={TextVariants.heading} className="mb-3">
          {t('adjustments.color.colorGrading')}
        </Text>
        <ColorGradingPanel
          adjustments={adjustments}
          setAdjustments={setAdjustments}
          appSettings={appSettings}
          onDragStateChange={onDragStateChange}
        />
      </div>

      {!isForMask && adjustmentVisibility.colorCalibration !== false && (
        <ColorCalibrationPanel
          adjustments={adjustments}
          setAdjustments={setAdjustments}
          appSettings={appSettings}
          onDragStateChange={onDragStateChange}
        />
      )}
    </div>
  );
}
// ========== BLITZRAW END: colour split into three panels ==========
