import { useTranslation } from 'react-i18next';
import Switch from './Switch';
import Button from './Button';
import Text from './Text';
import { TextVariants } from '../../types/typography';
import { ADJUSTMENT_GROUPS, COPYABLE_ADJUSTMENT_KEYS } from '../../utils/adjustments';

/**
 * Choosing which adjustments an operation carries.
 *
 * Shared between Copy and Paste and Save Preset, which is the point: a preset
 * and a paste both mean "take these settings and put them on another photo",
 * and there is no reason the two should disagree about what can be taken. The
 * preset dialog used to offer two switches, masks and crop, against this one's
 * full list, so anything outside those two was decided for you.
 *
 * Groups come from ADJUSTMENT_GROUPS, so a new adjustment appears in both
 * dialogs as soon as it is listed there once.
 */

const capitalize = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);

interface IncludedAdjustmentsProps {
  selected: Array<string>;
  onChange(next: Array<string>): void;
  /** Heading above the grid. */
  title: string;
  className?: string;
  /** How tall the scrolling grid may get. Wider dialogs can afford more. */
  gridHeightClass?: string;
}

export default function IncludedAdjustments({
  selected,
  onChange,
  title,
  className,
  gridHeightClass = 'max-h-64',
}: IncludedAdjustmentsProps) {
  const { t } = useTranslation();

  const toggleGroup = (keys: Array<string>, checked: boolean) => {
    const next = new Set(selected);
    keys.forEach((key) => (checked ? next.add(key) : next.delete(key)));
    onChange(Array.from(next));
  };

  return (
    <div className={className}>
      <div className="flex justify-between items-center mb-2">
        <Text variant={TextVariants.heading}>{title}</Text>
        <div className="flex gap-2">
          <Button
            className="px-4 py-2 rounded-md text-text-secondary hover:bg-surface transition-colors"
            size="sm"
            onClick={() => onChange([...COPYABLE_ADJUSTMENT_KEYS])}
          >
            {t('modals.copyPaste.selectAll')}
          </Button>
          <Button
            className="px-4 py-2 rounded-md text-text-secondary hover:bg-surface transition-colors"
            size="sm"
            onClick={() => onChange([])}
          >
            {t('modals.copyPaste.selectNone')}
          </Button>
        </div>
      </div>
      <div className={`bg-bg-primary p-4 rounded-md overflow-y-auto ${gridHeightClass}`}>
        <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-x-4 gap-y-6">
          {Object.entries(ADJUSTMENT_GROUPS).map(([section, groups]) => (
            <div key={section}>
              <Text variant={TextVariants.heading} className="mb-2">
                {t(`editor.adjustments.sections.${section}`, { defaultValue: capitalize(section) })}
              </Text>
              {groups.map((group) => (
                <div key={group.label} className="mb-1.5 last:mb-0">
                  <Switch
                    label={t(group.label)}
                    checked={group.keys.every((key) => selected.includes(key))}
                    onChange={(checked) => toggleGroup(group.keys, checked)}
                  />
                </div>
              ))}
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}

/**
 * What an older preset meant, in the terms the picker uses.
 *
 * Presets saved before this only recorded two booleans, so the rest of the
 * list has to be inferred: everything except the two groups they could turn
 * off. Without this an existing preset would open with nothing ticked and
 * quietly lose its contents on the next save.
 */
export function keysFromLegacyPresetFlags(includeMasks: boolean, includeCropTransform: boolean): Array<string> {
  const maskKeys = ADJUSTMENT_GROUPS.masks.flatMap((group) => group.keys);
  const geometryKeys = ADJUSTMENT_GROUPS.geometry.flatMap((group) => group.keys);

  return COPYABLE_ADJUSTMENT_KEYS.filter((key) => {
    if (!includeMasks && maskKeys.includes(key)) return false;
    if (!includeCropTransform && geometryKeys.includes(key)) return false;
    return true;
  });
}
