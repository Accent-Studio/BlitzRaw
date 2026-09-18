import { useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { toast } from 'react-toastify';

/**
 * Converting RAW files that no open source decoder can read.
 *
 * Nikon's High Efficiency modes are patented, so `rawler` refuses them and the
 * app silently falls back to the embedded JPEG preview. Adobe licenses the
 * codec, so running its free DNG Converter produces a negative that decodes
 * properly.
 *
 * Sources are never deleted. A `.dng` appears beside each RAW and the library's
 * format precedence then shows the DNG and hides the original.
 */

interface ConverterInfo {
  available: boolean;
  path: string | null;
  downloadUrl: string;
}

interface ConversionProgress {
  completed: number;
  total: number;
  current: string;
}

interface ConversionResult {
  source: string;
  output: string | null;
  outcome: 'converted' | 'skipped' | 'failed';
  message: string | null;
}

interface ConversionSummary {
  converted: number;
  skipped: number;
  failed: number;
  results: Array<ConversionResult>;
}

export function useDngConversion(onLibraryRefresh?: () => Promise<void> | void) {
  const convertPaths = useCallback(
    async (paths: Array<string>) => {
      if (paths.length === 0) {
        return;
      }

      // Virtual copies point at the same file on disk; convert each source once.
      const sources = [...new Set(paths.map((path) => path.split('?vc=')[0]))];

      const converter = await invoke<ConverterInfo>('find_dng_converter').catch(() => null);
      if (!converter?.available) {
        toast.error(
          'Adobe DNG Converter is not installed. It is a free download from Adobe, and is the only tool that can read Nikon High Efficiency files.',
          { autoClose: 10000 },
        );
        return;
      }

      // Phrased as a fraction rather than "converting N of M", which reads as
      // N remaining when it actually means N finished.
      const toastId = toast.loading(`Converting to DNG... 0/${sources.length}`);

      const unlisten = await listen<ConversionProgress>('dng-conversion-progress', (event) => {
        const { completed, total } = event.payload;
        toast.update(toastId, { render: `Converting to DNG... ${completed}/${total}` });
      });

      try {
        const summary = await invoke<ConversionSummary>('convert_to_dng', {
          paths: sources,
          overwrite: false,
        });

        const parts = [`${summary.converted} converted`];
        if (summary.skipped > 0) {
          parts.push(`${summary.skipped} already had a DNG`);
        }
        if (summary.failed > 0) {
          parts.push(`${summary.failed} failed`);
        }

        toast.update(toastId, {
          render: parts.join(', '),
          type: summary.failed > 0 ? 'warning' : 'success',
          isLoading: false,
          autoClose: 6000,
        });

        if (summary.failed > 0) {
          const firstFailure = summary.results.find((r) => r.outcome === 'failed');
          if (firstFailure?.message) {
            console.error('DNG conversion failures', summary.results.filter((r) => r.outcome === 'failed'));
            toast.error(`First failure: ${firstFailure.message}`, { autoClose: 10000 });
          }
        }

        if (summary.converted > 0) {
          await onLibraryRefresh?.();
        }
      } catch (err) {
        toast.update(toastId, {
          render: `DNG conversion failed: ${err}`,
          type: 'error',
          isLoading: false,
          autoClose: 10000,
        });
      } finally {
        unlisten();
      }
    },
    [onLibraryRefresh],
  );

  return { convertPaths };
}
