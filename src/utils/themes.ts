import { Theme } from '../components/ui/AppProperties';

export interface ThemeProps {
  cssVariables: any;
  id: Theme;
  name: string;
  splashImage: string;
}

export const THEMES: Array<ThemeProps> = [
  {
    id: Theme.Dark,
    name: 'settings.themes.dark',
    splashImage: '/splash-dark.jpg',
    cssVariables: {
      '--app-bg-primary': 'rgb(24, 24, 24)',
      '--app-bg-secondary': 'rgb(35, 35, 35)',
      '--app-surface': 'rgb(28, 28, 28)',
      '--app-card-active': 'rgb(43, 43, 43)',
      '--app-button-text': 'rgb(0, 0, 0)',
      '--app-text-primary': 'rgb(232, 234, 237)',
      '--app-text-secondary': 'rgb(158, 158, 158)',
      '--app-accent': 'rgb(255, 255, 255)',
      '--app-border-color': 'rgb(45, 45, 45)',
      '--app-hover-color': 'rgb(255, 255, 255)',
    },
  },
  {
    id: Theme.Light,
    name: 'settings.themes.light',
    splashImage: '/splash-light.jpg',
    cssVariables: {
      '--app-bg-primary': 'rgb(245, 245, 245)',
      '--app-bg-secondary': 'rgb(255, 255, 255)',
      '--app-surface': 'rgb(241, 241, 241)',
      '--app-card-active': 'rgb(250, 250, 250)',
      '--app-button-text': 'rgb(255, 255, 255)',
      '--app-text-primary': 'rgb(20, 20, 20)',
      '--app-text-secondary': 'rgb(108, 108, 108)',
      '--app-accent': 'rgb(198, 142, 110)',
      '--app-border-color': 'rgb(224, 224, 224)',
      '--app-hover-color': 'rgb(198, 142, 110)',
    },
  },
  {
    id: Theme.Grey,
    name: 'settings.themes.grey',
    splashImage: '/splash-grey.jpg',
    cssVariables: {
      '--app-bg-primary': 'rgb(112, 112, 112)',
      '--app-bg-secondary': 'rgb(118, 118, 118)',
      '--app-surface': 'rgb(108, 108, 108)',
      '--app-card-active': 'rgb(133, 133, 133)',
      '--app-button-text': 'rgb(45, 45, 45)',
      '--app-text-primary': 'rgb(240, 240, 240)',
      '--app-text-secondary': 'rgb(180, 180, 180)',
      '--app-accent': 'rgb(220, 220, 220)',
      '--app-border-color': 'rgb(138, 138, 138)',
      '--app-hover-color': 'rgb(220, 220, 220)',
    },
  },
];

export const DEFAULT_THEME_ID = Theme.Dark;

/**
 * Paints a theme onto the document, and the font with it.
 *
 * Lifted out of `useAppInitialization` so that a window which is not the whole
 * application can still have colours. Every colour class in the app resolves
 * through `--app-*`, and nothing in the stylesheet gives those a value: they
 * are set here and only here. A window that never runs this has no background
 * and no text colour, which on WebView2 is a rectangle of plain white. That is
 * exactly what a detached panel window looked like, and it looked identical to
 * a page that had failed to load, which is what sent two attempts at it looking
 * in the wrong place.
 */
export function applyTheme(themeId: string | null | undefined, fontFamily?: string | null): void {
  if (typeof document === 'undefined') {
    return;
  }
  const root = document.documentElement;
  const base =
    THEMES.find((theme: ThemeProps) => theme.id === (themeId || DEFAULT_THEME_ID)) ||
    THEMES.find((theme: ThemeProps) => theme.id === DEFAULT_THEME_ID);
  if (!base) {
    return;
  }

  for (const [key, value] of Object.entries(base.cssVariables)) {
    root.style.setProperty(key, value as string);
  }

  root.style.setProperty(
    '--font-family',
    fontFamily === 'system'
      ? '-apple-system, BlinkMacSystemFont, system-ui, sans-serif'
      : "'Poppins', system-ui, sans-serif",
  );
}
