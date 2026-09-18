import { GroupPreference, ImageFile } from '../components/ui/AppProperties';

export type GroupId = string;

export interface GroupBadgeInfo {
  count: number;
  label: string;
}

export interface GroupingResult {
  displayList: ImageFile[];
  badges: Map<GroupId, GroupBadgeInfo>;
}

export function buildImageGroups(
  images: ImageFile[],
  preference: GroupPreference,
  groupEditedFiles = true,
): GroupingResult {
  const buckets = new Map<GroupId, ImageFile[]>();

  for (const image of images) {
    if (!image.group_id || image.is_virtual_copy) continue;
    if (!groupEditedFiles && image.is_edited) continue;

    let bucket = buckets.get(image.group_id);
    if (!bucket) {
      bucket = [];
      buckets.set(image.group_id, bucket);
    }
    bucket.push(image);
  }

  const groupedPaths = new Set<string>();
  const badges = new Map<GroupId, GroupBadgeInfo>();

  for (const [groupId, files] of buckets) {
    if (files.length < 2) continue;

    const primary = pickPrimary(files, preference);
    for (const file of files) {
      if (file.path !== primary.path) {
        groupedPaths.add(file.path);
      }
    }

    const extensions = new Set(files.map((f) => getVariantLabel(f.path)));
    badges.set(groupId, {
      count: files.length,
      label: Array.from(extensions).sort().join('+'),
    });
  }

  const displayList = images.filter((img) => !groupedPaths.has(img.path));
  return { displayList, badges };
}

/**
 * Picks which RAW represents a capture that exists as more than one RAW file.
 *
 * A DNG sitting beside a NEF is normally the result of converting that NEF,
 * and for Nikon High Efficiency files it is the only one any open source
 * decoder can actually develop. Preferring it means the group shows the file
 * the conversion was run to produce, rather than whichever happened to be
 * enumerated first.
 */
function pickRaw(files: ImageFile[]): ImageFile | undefined {
  const raws = files.filter((f) => f.is_raw);
  return raws.find((f) => getFileExtension(f.path) === 'dng') ?? raws[0];
}

function pickPrimary(files: ImageFile[], preference: GroupPreference): ImageFile {
  const raw = pickRaw(files);
  const nonRaw = files.find((f) => !f.is_raw);

  switch (preference) {
    case 'raw':
      return raw ?? nonRaw ?? files[0];
    case 'jpeg':
      return nonRaw ?? raw ?? files[0];
    default:
      return files[0];
  }
}

export function getFileExtension(path: string): string {
  const clean = path.split('?')[0];
  const dot = clean.lastIndexOf('.');
  if (dot === -1) return '';
  return clean.substring(dot + 1).toLowerCase();
}

export function getVariantLabel(path: string): string {
  const ext = getFileExtension(path);
  return ext ? ext.toUpperCase() : 'FILE';
}

export function findGroupVariants(images: ImageFile[], groupId: string | null | undefined): ImageFile[] {
  if (!groupId) return [];
  return images.filter((img) => img.group_id === groupId && !img.is_virtual_copy);
}
