import { useEffect, useState, useCallback } from 'react';
import { useTranslation } from 'react-i18next';
import Button from '../ui/Button';
import Text from '../ui/Text';
import { TextVariants } from '../../types/typography';

/** An optional toggle shown above the buttons, for a choice that belongs to the
 *  action being confirmed rather than to a settings panel. */
export interface ConfirmModalCheckbox {
  label: string;
  checked: boolean;
  onChange(checked: boolean): void;
}

interface ConfirmModalProps {
  cancelText?: string;
  checkbox?: ConfirmModalCheckbox;
  confirmText?: string;
  confirmVariant?: string;
  /** BLITZRAW: a word that has to be typed before Confirm will do anything.
   *  For the few actions that take away more than one thing and cannot be put
   *  back by pressing Ctrl+Z. A click lands where the mouse already was; typing
   *  a word does not happen by accident. */
  confirmPhrase?: string;
  isOpen: boolean;
  message?: string;
  onClose(): void;
  onConfirm?(): void;
  title?: string;
}

export default function ConfirmModal({
  cancelText,
  checkbox,
  confirmText,
  confirmVariant = 'primary',
  confirmPhrase,
  isOpen,
  message,
  onClose,
  onConfirm,
  title,
}: ConfirmModalProps) {
  const { t } = useTranslation();
  const [isMounted, setIsMounted] = useState(false);
  const [show, setShow] = useState(false);
  const [typed, setTyped] = useState('');
  const isUnlocked = !confirmPhrase || typed.trim().toLowerCase() === confirmPhrase.toLowerCase();

  const resolvedCancelText = cancelText || t('modals.confirm.cancel');
  const resolvedConfirmText = confirmText || t('modals.confirm.confirm');

  useEffect(() => {
    if (isOpen) {
      // BLITZRAW: cleared on every open, so the word typed last time cannot
      // still be sitting there waiting for a stray Enter.
      setTyped('');
      setIsMounted(true);
      const timer = setTimeout(() => {
        setShow(true);
      }, 10);
      return () => clearTimeout(timer);
    } else {
      setShow(false);
      const timer = setTimeout(() => {
        setIsMounted(false);
      }, 300);
      return () => clearTimeout(timer);
    }
  }, [isOpen]);

  const handleConfirm = useCallback(() => {
    // BLITZRAW: Enter goes through here too, so the gate belongs here rather
    // than only on the button.
    if (!isUnlocked) {
      return;
    }
    if (onConfirm) {
      onConfirm();
    }
    onClose();
  }, [isUnlocked, onConfirm, onClose]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLDivElement>) => {
      if (e.key === 'Enter') {
        e.preventDefault();
        e.stopPropagation();
        e.nativeEvent.stopImmediatePropagation();
        handleConfirm();
      } else if (e.key === 'Escape') {
        e.preventDefault();
        e.stopPropagation();
        e.nativeEvent.stopImmediatePropagation();
        onClose();
      }
    },
    [handleConfirm, onClose],
  );

  if (!isMounted) {
    return null;
  }

  return (
    <div
      aria-labelledby="confirm-modal-title"
      aria-modal="true"
      className={`
        fixed inset-0 flex items-center justify-center z-50
        bg-black/30 backdrop-blur-xs
        transition-opacity duration-300 ease-in-out
        ${show ? 'opacity-100' : 'opacity-0'}
      `}
      onClick={onClose}
      role="dialog"
    >
      <div
        className={`
          bg-surface rounded-lg shadow-xl p-6 w-full max-w-md
          transform transition-all duration-300 ease-out
          ${show ? 'scale-100 opacity-100 translate-y-0' : 'scale-95 opacity-0 -translate-y-4'}
        `}
        onClick={(e: any) => e.stopPropagation()}
        onKeyDown={handleKeyDown}
      >
        <Text variant={TextVariants.title} id="confirm-modal-title" className="mb-4">
          {title}
        </Text>
        <Text className="mb-6 whitespace-pre-wrap">{message}</Text>
        {checkbox && (
          <label className="flex items-center gap-2 cursor-pointer select-none text-text-secondary hover:text-text-primary transition-colors">
            <input
              type="checkbox"
              className="accent-accent w-4 h-4 cursor-pointer"
              checked={checkbox.checked}
              onChange={(e: any) => checkbox.onChange(e.target.checked)}
            />
            <Text variant={TextVariants.small}>{checkbox.label}</Text>
          </label>
        )}
        {confirmPhrase && (
          <div className="mt-4">
            <Text variant={TextVariants.small} className="mb-2">
              {t('modals.confirm.typeToConfirm', { phrase: confirmPhrase })}
            </Text>
            <input
              type="text"
              autoFocus={true}
              spellCheck={false}
              autoComplete="off"
              value={typed}
              onChange={(e: any) => setTyped(e.target.value)}
              className="w-full bg-bg-primary text-text-primary rounded-md px-3 py-2 font-mono
                         outline-hidden focus:ring-2 focus:ring-accent"
            />
          </div>
        )}
        <div className="flex justify-end gap-3 mt-5">
          <Button
            className="bg-bg-primary shadow-transparent hover:bg-bg-primary text-white shadow-none focus:outline-hidden focus:ring-0"
            onClick={onClose}
            variant="ghost"
            tabIndex={0}
          >
            {resolvedCancelText}
          </Button>
          <Button
            onClick={handleConfirm}
            variant={confirmVariant}
            disabled={!isUnlocked}
            // BLITZRAW: the typing box takes the focus when there is one, so
            // Enter cannot fire a destructive button nobody looked at.
            autoFocus={!confirmPhrase}
            className={`focus:outline-hidden focus:ring-0 focus:ring-offset-0 ${
              isUnlocked ? '' : 'opacity-40 cursor-not-allowed'
            }`}
          >
            {resolvedConfirmText}
          </Button>
        </div>
      </div>
    </div>
  );
}
