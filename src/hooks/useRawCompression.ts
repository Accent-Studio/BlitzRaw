import { useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useLibraryStore } from '../store/useLibraryStore';

/**
 * Finding out which RAW files cannot actually be developed.
 *
 * Nikon's High Efficiency modes use a patented codec that no open source
 * decoder implements. Faced with one, the app falls back to the JPEG preview
 * the camera embedded and carries on, so the picture looks fine while the
 * editing controls are working on 8-bit camera-processed data rather than
 * sensor data. Nothing on screen says so.
 *
 * This probes the folder's RAW files, cheaply and off the critical path, so the
 * grid can mark the ones that need converting before they are edited.
 */

interface CompressionReport {
  path: string;
  label: string | null;
  needsConversion: boolean;
}

/** Extensions whose compression can be undecodable. Probing anything else is wasted work. */
const PROBEABLE = ['.nef', '.nrw'];

function isProbeable(path: string): boolean {
  const clean = path.split('?')[0].toLowerCase();
  return PROBEABLE.some((ext) => clean.endsWith(ext));
}

export function useRawCompression(imageList: Array<{ path: string }>) {
  // Paths already asked about, so switching folders back and forth does not
  // re-read headers it has already seen.
  const probed = useRef<Set<string>>(new Set());

  useEffect(() => {
    const pending = imageList
      .map((image) => image.path.split('?vc=')[0])
      .filter((path) => isProbeable(path) && !probed.current.has(path));

    const unique = [...new Set(pending)];
    if (unique.length === 0) {
      return;
    }

    let cancelled = false;

    // Deferred so a folder change does not pay for this before its thumbnails.
    const timer = setTimeout(() => {
      invoke<Array<CompressionReport>>('probe_raw_compression', { paths: unique })
        .then((reports) => {
          if (cancelled) return;
          unique.forEach((path) => probed.current.add(path));

          const additions: Record<string, { label: string | null; needsConversion: boolean }> = {};
          for (const report of reports) {
            additions[report.path] = {
              label: report.label,
              needsConversion: report.needsConversion,
            };
          }

          useLibraryStore.getState().setLibrary((state: any) => ({
            rawCompression: { ...state.rawCompression, ...additions },
          }));
        })
        .catch((err) => {
          // Not knowing is the status quo, so a failure here is not worth
          // interrupting anyone over.
          console.error('Could not probe RAW compression', err);
        });
    }, 400);

    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [imageList]);
}
