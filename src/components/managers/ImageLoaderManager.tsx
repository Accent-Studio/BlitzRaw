import { useImageLoader } from '../../hooks/useImageLoader';
import { useProxyPreview } from '../../hooks/useProxyPreview';

interface Props {
  cachedEditStateRef: React.RefObject<any>;
  prevAdjustmentsRef: React.RefObject<any>;
}

export default function ImageLoaderManager({ cachedEditStateRef, prevAdjustmentsRef }: Props) {
  useImageLoader(cachedEditStateRef, prevAdjustmentsRef);
  // BLITZRAW: moves the picture while the raw decodes. Mounted here because
  // this is where the photo's own loading already lives.
  useProxyPreview();

  return null;
}
