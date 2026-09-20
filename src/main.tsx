import React from 'react';
import { createRoot } from 'react-dom/client';
import App from './App';
import FloatingPanels from './window/FloatingPanels';
import { installFrontendLogBridge } from './utils/frontendLogBridge';
import { applyTheme } from './utils/themes';
import './styles.css';

installFrontendLogBridge();

// A window opened by `open_panel_window` carries the panel it is for in its
// query. Everything else is the application proper. Branching here rather than
// inside App keeps the detached window from mounting the library, the editor
// and every listener they install, none of which it has any use for.
const detachedPanel = new URLSearchParams(window.location.search).get('panel');

if (detachedPanel) {
  // The default theme, before React is asked to do anything at all. The
  // application proper sets this from an effect once settings have loaded, and
  // a detached window that waited for the same thing would be a white
  // rectangle until then, which is precisely how this feature failed twice:
  // nothing in the stylesheet gives the `--app-*` properties a value, so a
  // page without them paints nothing and WebView2 shows plain white. Painted
  // here, no failure further in can produce that again, and the settings that
  // arrive a moment later only correct the choice of theme.
  applyTheme(null);
}

const root = createRoot(document.getElementById('root')!);
root.render(
  <React.StrictMode>{detachedPanel ? <FloatingPanels /> : <App />}</React.StrictMode>,
);
