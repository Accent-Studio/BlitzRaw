import { useState, useEffect, useCallback, useRef, useMemo } from 'react';
import { motion } from 'framer-motion';
import { useTranslation } from 'react-i18next';
import clsx from 'clsx';
import Text from '../ui/Text';
import { TextVariants } from '../../types/typography';
import Switch from '../ui/Switch';
import { Preset } from '../ui/AppProperties';
import { ADJUSTMENT_GROUPS, COPYABLE_ADJUSTMENT_KEYS } from '../../utils/adjustments';
import IncludedAdjustments, { keysFromLegacyPresetFlags } from '../ui/IncludedAdjustments';

interface ConfigurePresetModalProps {
  isOpen: boolean;
  onClose(): void;
  onSave(name: string, includedAdjustments: Array<string>, presetType: 'tool' | 'style'): void;
  initialPreset?: Preset | null;
}

interface PresetTypeSwitchProps {
  selectedType: 'tool' | 'style';
  onChange: (type: 'tool' | 'style') => void;
}

const PresetTypeSwitch = ({ selectedType, onChange }: PresetTypeSwitchProps) => {
  const { t } = useTranslation();
  const [bubbleStyle, setBubbleStyle] = useState({});
  const isInitialAnimation = useRef(true);

  const presetTypeOptions = useMemo(
    () => [
      {
        id: 'style' as const,
        label: t('modals.configurePreset.typeStyleLabel'),
        title: t('modals.configurePreset.typeStyleDesc'),
      },
      {
        id: 'tool' as const,
        label: t('modals.configurePreset.typeToolLabel'),
        title: t('modals.configurePreset.typeToolDesc'),
      },
    ],
    [t],
  );

  useEffect(() => {
    const selectedIndex = presetTypeOptions.findIndex((m) => m.id === selectedType);
    const safeIndex = selectedIndex >= 0 ? selectedIndex : 0;

    const widthPercent = 100 / presetTypeOptions.length;
    const targetX = `${safeIndex * 100}%`;
    const targetWidth = `${widthPercent}%`;

    if (isInitialAnimation.current) {
      let initialX = selectedType === 'style' ? '-25%' : '100%';

      setBubbleStyle({
        x: [initialX, targetX],
        width: targetWidth,
      });
      isInitialAnimation.current = false;
    } else {
      setBubbleStyle({
        x: targetX,
        width: targetWidth,
      });
    }
  }, [selectedType, presetTypeOptions]);

  return (
    <div className="w-full p-1.5 bg-card-active rounded-md mt-2">
      <div className="relative flex w-full">
        <motion.div
          className="absolute top-0 bottom-0 z-0 bg-accent"
          style={{ borderRadius: 4 }}
          animate={bubbleStyle}
          transition={{ type: 'spring', bounce: 0.2, duration: 0.6 }}
        />
        {presetTypeOptions.map((option) => (
          <button
            key={option.id}
            data-tooltip={option.title}
            onClick={(e) => {
              e.preventDefault();
              onChange(option.id);
            }}
            className={clsx(
              'relative flex-1 flex items-center justify-center gap-2 px-3 py-1.5 text-sm font-medium rounded-md transition-colors',
              {
                'text-text-primary hover:bg-surface': selectedType !== option.id,
                'text-button-text': selectedType === option.id,
              },
            )}
            style={{ WebkitTapHighlightColor: 'transparent' }}
          >
            <span className="relative z-10 flex items-center">{option.label}</span>
          </button>
        ))}
      </div>
    </div>
  );
};

export default function ConfigurePresetModal({ isOpen, onClose, onSave, initialPreset }: ConfigurePresetModalProps) {
  const { t } = useTranslation();
  const [name, setName] = useState('');
  // A full list rather than two switches, matching Copy and Paste. Anything
  // outside masks and crop used to be decided for you, which is how lens
  // correction ended up unreachable from a preset.
  const [included, setIncluded] = useState<Array<string>>([...COPYABLE_ADJUSTMENT_KEYS]);
  const [presetType, setPresetType] = useState<'tool' | 'style'>('style');
  const [isMounted, setIsMounted] = useState(false);
  const [show, setShow] = useState(false);

  useEffect(() => {
    if (isOpen) {
      setName(initialPreset?.name || '');

      if (initialPreset?.includedAdjustments?.length) {
        setIncluded(initialPreset.includedAdjustments);
      } else if (initialPreset) {
        // Saved before the list existed, so read its two switches back. A
        // preset opening with nothing ticked would lose its contents on save.
        const GEOMETRY_KEYS = ADJUSTMENT_GROUPS.geometry.flatMap((group) => group.keys);
        const hasGeometry = !!(
          initialPreset.adjustments &&
          Object.keys(initialPreset.adjustments).some((key) => GEOMETRY_KEYS.includes(key))
        );
        const hasMasks = !!(initialPreset.adjustments?.masks && initialPreset.adjustments.masks.length > 0);
        setIncluded(
          keysFromLegacyPresetFlags(
            initialPreset.includeMasks ?? hasMasks,
            initialPreset.includeCropTransform ?? hasGeometry,
          ),
        );
      } else {
        setIncluded([...COPYABLE_ADJUSTMENT_KEYS]);
      }

      setPresetType(initialPreset?.presetType || 'style');
      setIsMounted(true);
      const timer = setTimeout(() => setShow(true), 10);
      return () => clearTimeout(timer);
    } else {
      setShow(false);
      const timer = setTimeout(() => {
        setIsMounted(false);
        setName('');
        setIncluded([...COPYABLE_ADJUSTMENT_KEYS]);
        setPresetType('style');
      }, 300);
      return () => clearTimeout(timer);
    }
  }, [isOpen, initialPreset]);

  const handleSave = useCallback(() => {
    if (name.trim()) {
      onSave(name.trim(), included, presetType);
      onClose();
    }
  }, [name, included, presetType, onSave, onClose]);

  const handleKeyDown = useCallback(
    (e: any) => {
      if (e.key === 'Enter') {
        handleSave();
      } else if (e.key === 'Escape') {
        onClose();
      }
    },
    [handleSave, onClose],
  );

  if (!isMounted) {
    return null;
  }

  return (
    <div
      className={`
        fixed inset-0 flex items-center justify-center z-50
        bg-black/30 backdrop-blur-xs
        transition-opacity duration-300 ease-in-out
        ${show ? 'opacity-100' : 'opacity-0'}
      `}
      onClick={onClose}
      role="dialog"
      aria-modal="true"
    >
      <div
        className={`
          bg-surface rounded-lg shadow-xl p-6 w-full max-w-3xl
          transform transition-all duration-300 ease-out
          ${show ? 'scale-100 opacity-100 translate-y-0' : 'scale-95 opacity-0 -translate-y-4'}
        `}
        onClick={(e: any) => e.stopPropagation()}
      >
        <Text variant={TextVariants.title} className="mb-4">
          {initialPreset ? t('modals.configurePreset.titleConfigure') : t('modals.configurePreset.titleSave')}
        </Text>
        <input
          autoFocus
          className="w-full bg-bg-primary text-text-primary border border-border rounded-md px-3 py-2 focus:outline-none focus:ring-2 focus:ring-accent"
          onChange={(e: any) => setName(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder={t('modals.configurePreset.placeholder')}
          type="text"
          value={name}
        />

        <IncludedAdjustments
          className="mt-5 mb-4"
          gridHeightClass="max-h-[26rem]"
          title={t('modals.configurePreset.includedAdjustments')}
          selected={included}
          onChange={setIncluded}
        />

        <PresetTypeSwitch selectedType={presetType} onChange={setPresetType} />

        <div className="flex justify-end gap-3 mt-6">
          <button
            className="px-4 py-2 rounded-md text-text-secondary hover:bg-surface transition-colors"
            onClick={onClose}
          >
            {t('modals.configurePreset.cancel')}
          </button>
          <button
            className="px-4 py-2 rounded-md bg-accent text-button-text font-semibold hover:bg-accent-hover disabled:bg-gray-500 disabled:text-white disabled:cursor-not-allowed transition-colors"
            disabled={!name.trim()}
            onClick={handleSave}
          >
            {t('modals.configurePreset.save')}
          </button>
        </div>
      </div>
    </div>
  );
}
