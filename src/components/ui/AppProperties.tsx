import { ExportPreset } from './ExportImportProperties';
import { Adjustments, CopyPasteSettings } from '../../utils/adjustments';
import { ToolType } from '../panel/right/Masks';

export const GLOBAL_KEYS = [
  ' ',
  'ArrowUp',
  'ArrowDown',
  'ArrowLeft',
  'ArrowRight',
  'f',
  'b',
  'a',
  's',
  'd',
  'r',
  'm',
  'k',
  'p',
  'i',
  'e',
  '0',
  '1',
  '2',
  '3',
  '4',
  '5',
  'Enter',
];
export const OPTION_SEPARATOR = 'separator';

/** BLITZRAW: which denoiser runs. `scunet` is the strongest and the default
 *  for raw files; `ai` is the older NIND model; `bm3d` is the one that needs no
 *  model at all and runs on the processor. */
export type DenoiseMethod = 'scunet' | 'ai' | 'bm3d';

export enum Invokes {
  AddTagForPaths = 'add_tag_for_paths',
  ApplyAdjustments = 'apply_adjustments',
  ApplyAdjustmentsToPaths = 'apply_adjustments_to_paths',
  ApplyAutoAdjustmentsToPaths = 'apply_auto_adjustments_to_paths',
  ApplyDenoising = 'apply_denoising',
  /** BLITZRAW: one square of the picture, cleaned, at full size. */
  DenoisePreviewPatch = 'denoise_preview_patch',
  CalculateAutoAdjustments = 'calculate_auto_adjustments',
  CancelExport = 'cancel_export',
  CheckAIConnectorStatus = 'check_ai_connector_status',
  ClearAllSidecars = 'clear_all_sidecars',
  ClearAiTags = 'clear_ai_tags',
  ClearAllTags = 'clear_all_tags',
  ClearThumbnailCache = 'clear_thumbnail_cache',
  CopyFiles = 'copy_files',
  CreateFolder = 'create_folder',
  CreateVirtualCopy = 'create_virtual_copy',
  CullImages = 'cull_images',
  DeleteFolder = 'delete_folder',
  DuplicateFile = 'duplicate_file',
  EstimateExportSizes = 'estimate_export_sizes',
  ExportImages = 'export_images',
  PinExportedState = 'pin_exported_state',
  ScopesFromSmallPicture = 'scopes_from_small_picture',
  FrontendLog = 'frontend_log',
  GenerateAiForegroundMask = 'generate_ai_foreground_mask',
  GenerateAiSkyMask = 'generate_ai_sky_mask',
  GenerateAiSubjectMask = 'generate_ai_subject_mask',
  GenerateFullscreenPreview = 'generate_fullscreen_preview',
  GeneratePreviewForPath = 'generate_preview_for_path',
  GenerateMaskOverlay = 'generate_mask_overlay',
  GeneratePresetPreview = 'generate_preset_preview',
  GenerateThumbnailsProgressive = 'generate_thumbnails_progressive',
  NudgeAdjustmentsForPaths = 'nudge_adjustments_for_paths',
  GoToSteps = 'go_to_steps',
  RenderNudgedPreview = 'render_nudged_preview',
  ForgetNudgedSource = 'forget_nudged_source',
  BuildPreviewsForPaths = 'build_previews_for_paths',
  DiscardPreviewsForPaths = 'discard_previews_for_paths',
  CachedPreviewForPath = 'cached_preview_for_path',
  CountCachedPreviews = 'count_cached_previews',
  GenerateUncroppedPreview = 'generate_uncropped_preview',
  GetFolderTree = 'get_folder_tree',
  GetFolderChildren = 'get_folder_children',
  GetLogFilePath = 'get_log_file_path',
  GetOrCreateInternalLibraryRoot = 'get_or_create_internal_library_root',
  GetPinnedFolderTrees = 'get_pinned_folder_trees',
  GetSupportedFileTypes = 'get_supported_file_types',
  HandleExportPresetsToFile = 'handle_export_presets_to_file',
  HandleImportPresetsFromFile = 'handle_import_presets_from_file',
  HandleImportPresetsFromFiles = 'handle_import_presets_from_files',
  HandleImportLegacyPresetsFromFile = 'handle_import_legacy_presets_from_file',
  ImportFiles = 'import_files',
  InvokeGenerativeReplace = 'invoke_generative_replace',
  InvokeGenerativeReplaseWithMaskDef = 'invoke_generative_replace_with_mask_def',
  ListImagesInDir = 'list_images_in_dir',
  ListImagesRecursive = 'list_images_recursive',
  LoadImage = 'load_image',
  LoadMetadata = 'load_metadata',
  LoadPresets = 'load_presets',
  LoadSettings = 'load_settings',
  MoveFiles = 'move_files',
  ReadExifForPaths = 'read_exif_for_paths',
  RemoveTagForPaths = 'remove_tag_for_paths',
  RenameFiles = 'rename_files',
  RenameFolder = 'rename_folder',
  ResetAdjustmentsForPaths = 'reset_adjustments_for_paths',
  SaveMetadataAndUpdateThumbnail = 'save_metadata_and_update_thumbnail',
  SaveCollage = 'save_collage',
  SaveDenoisedImage = 'save_denoised_image',
  SavePanorama = 'save_panorama',
  SaveHdr = 'save_hdr',
  SavePresets = 'save_presets',
  SaveSettings = 'save_settings',
  SetColorLabelForPaths = 'set_color_label_for_paths',
  SetRatingForPaths = 'set_rating_for_paths',
  ShowInFinder = 'show_in_finder',
  StartBackgroundIndexing = 'start_background_indexing',
  StitchPanorama = 'stitch_panorama',
  MergeHdr = 'merge_hdr',
  TestAIConnectorConnection = 'test_ai_connector_connection',
  UpdateWgpuTransform = 'update_wgpu_transform',
  UpdateExifFields = 'update_exif_fields',
  FetchCommunityPresets = 'fetch_community_presets',
  GenerateAllCommunityPreviews = 'generate_all_community_previews',
  SaveCommunityPreset = 'save_community_preset',
  SaveTempFile = 'save_temp_file',
  GetAlbums = 'get_albums',
  SaveAlbums = 'save_albums',
  AddToAlbum = 'add_to_album',
  GetAlbumImages = 'get_album_images',
}

export enum ExifOverlay {
  Off = 'off',
  Hover = 'hover',
  Always = 'always',
}

export enum Panel {
  Adjustments = 'adjustments',
  Ai = 'ai',
  Crop = 'crop',
  Export = 'export',
  Masks = 'masks',
  Metadata = 'metadata',
  Presets = 'presets',
  FolderTree = 'folderTree',
  Scopes = 'scopes',
  Navigator = 'navigator',
  History = 'history',
}

/**
 * Where a panel lives.
 *
 * The four sidebar regions, plus two that are not in the main window at all:
 * `floatTop` and `floatBottom` are the column inside the floating window. They
 * are regions like any other on purpose, so the tab strips, the drag and drop,
 * the switcher placement and the saved workspace all work there without a
 * second implementation of any of it. The main window simply never draws them.
 */
export type PanelRegion = 'leftTop' | 'leftBottom' | 'rightTop' | 'rightBottom' | 'floatTop' | 'floatBottom';

/** The regions that belong to the floating window rather than the sidebars. */
export const FLOATING_REGIONS: Array<PanelRegion> = ['floatTop', 'floatBottom'];

export function isFloatingRegion(region: PanelRegion): boolean {
  return FLOATING_REGIONS.includes(region);
}

export enum RawStatus {
  All = 'all',
  NonRawOnly = 'nonRawOnly',
  RawOnly = 'rawOnly',
}

export enum SortDirection {
  Ascending = 'asc',
  Descending = 'desc',
}

export type FolderSortKey = 'name' | 'modified' | 'created' | 'imageCount';

export interface FolderTreeSort {
  key: FolderSortKey;
  order: SortDirection;
}

export enum Theme {
  Arctic = 'arctic',
  Blue = 'blue',
  Dark = 'dark',
  Grey = 'grey',
  Light = 'light',
  MutedGreen = 'muted-green',
  Sepia = 'sepia',
  Snow = 'snow',
}

export enum ThumbnailAspectRatio {
  Cover = 'cover',
  Contain = 'contain',
}

export interface WorkspaceState {
  leftPanelWidth: number;
  rightPanelWidth: number;
  leftTopHeight: number;
  rightTopHeight: number;
  panelLayout: Record<PanelRegion, Panel[]>;
  activePanels: Record<PanelRegion, Panel | null>;
  panelSwitcherPlacement: Record<PanelRegion, 'left' | 'right' | 'top' | 'bottom'>;
}

export type GroupPreference = 'jpeg' | 'raw';
export type GroupingMode = 'off' | GroupPreference;

export interface AppSettings {
  aiConnectorAddress?: string;
  aiProvider?: string;
  decorations?: any;
  editorPreviewResolution?: number;
  enableZoomHifi?: boolean;
  useFullDpiRendering?: boolean;
  highResZoomMultiplier?: number;
  enableLivePreviews?: boolean;
  livePreviewQuality?: string;
  enableAiTagging?: boolean;
  aiTagCount?: number;
  customAiTags?: string[];
  filterCriteria?: FilterCriteria;
  lastFolderState?: any;
  pinnedFolders?: any;
  lastRootPath: string | null;
  rootFolders?: string[];
  libraryViewMode?: LibraryViewMode;
  sortCriteria?: SortCriteria;
  theme: Theme;
  thumbnailSize?: ThumbnailSize;
  /**
   * BLITZRAW: the grid's minimum cell width in pixels. Written alongside the
   * old thumbnailSize rather than replacing it, so a settings file stays
   * readable by a build that predates the slider.
   */
  thumbnailSizePx?: number;
  thumbnailAspectRatio?: ThumbnailAspectRatio;
  uiVisibility?: UiVisibility;
  adjustmentVisibility?: { [key: string]: boolean };
  rawHighlightCompression?: number;
  processingBackend?: string;
  linuxGpuOptimization?: boolean;
  exportPresets?: ExportPreset[];
  myLenses?: any;
  enableFolderImageCounts?: boolean;
  displayEditIcon?: boolean;
  linearRawMode?: string;
  enableXmpSync?: boolean;
  createXmpIfMissing?: boolean;
  isWaveformVisible?: boolean;
  waveformHeight?: number;
  /** Layouts the user asked to keep, by name. Separate from the one that autosaves. */
  layoutProfiles?: Array<{ name: string; workspace: unknown }>;
  activeWaveformChannel?: string;
  waveformChannels?: Array<string>;
  vectorscopeGain?: number;
  scopeHeights?: Array<number>;
  /** How many edit steps each photo keeps. See editHistory.ts. */
  historyStepLimit?: number;
  useWgpuRenderer?: boolean;
  canvasInputMode?: 'mouse' | 'trackpad';
  zoomSpeedMultiplier?: number;
  /**
   * Sliders added to Quick Adjustments by right-clicking them. The three that
   * ship are not stored here; only what the user added.
   */
  quickAdjustments?: Array<{ id: string; path: string; step: number; min: number; max: number; label?: string }>;
  /**
   * Which sections of the Adjustments panel are left open. A preference about
   * the panel, not about a photo, so it is kept here rather than in a sidecar.
   */
  openAdjustmentSections?: { [section: string]: boolean };
  keybinds?: { [action: string]: string[] };
  tonemapperOverrideEnabled?: boolean;
  defaultRawTonemapper?: string;
  defaultNonRawTonemapper?: string;
  copyPasteSettings?: CopyPasteSettings;
  enableFocusMode?: boolean;
  openTreeSections?: string[];
  folderIcons?: Record<string, string>;
  exifOverlay?: ExifOverlay;
  language?: string;
  fontFamily?: string;
  folderTreeSort?: FolderTreeSort;
  taggingShortcuts?: string[];
  libraryDisplayMode?: LibraryDisplayMode;
  grouping?: GroupingMode;
  /** Skip the splash screen and reopen the last folder on launch. */
  autoContinueSession?: boolean;
  /** Align bracket frames before merging to HDR. Off by default; see the merge
   *  confirmation, which carries the toggle and remembers the choice. */
  hdrAutoAlign?: boolean;
  requireMatchingExif?: boolean;
  groupEditedFiles?: boolean;
  groupPreferredType?: GroupPreference; // legacy
  alwaysDecodeRawThumbnails?: boolean;
  workspace?: WorkspaceState;
}

export interface BrushSettings {
  feather: number;
  size: number;
  tool: ToolType;
}

export enum LibraryViewMode {
  Flat = 'flat',
  Recursive = 'recursive',
}

export const EditedStatus = {
  All: 'all',
  EditedOnly: 'editedOnly',
  UneditedOnly: 'uneditedOnly',
} as const;

export type EditedStatus = (typeof EditedStatus)[keyof typeof EditedStatus];

export interface FilterCriteria {
  colors: Array<string>;
  rating: number;
  rawStatus: RawStatus;
  editedStatus?: EditedStatus;
}

export interface Folder {
  children: any;
  id?: string | undefined;
  name?: string | undefined;
  imageCount?: number;
}

export interface ImageFile {
  is_edited: boolean;
  modified: number;
  path: string;
  rating: number;
  tags: Array<string> | null;
  exif: { [key: string]: string } | null;
  is_virtual_copy: boolean;
  is_cloud_placeholder: boolean;
  is_raw: boolean;
  /** Ties together several files of one capture, e.g. a NEF and its JPEG. */
  group_id: string | null;
  /** Ties together several captures the user stacked, e.g. an exposure bracket. */
  stack_id?: string | null;
  /** The frame shown when its stack is closed, where one was chosen explicitly. */
  is_stack_leader?: boolean;
}

export interface Option {
  color?: string;
  disabled?: boolean;
  icon?: any;
  isDestructive?: boolean;
  label?: string;
  onClick?(): void;
  onRightClick?(): void;
  submenu?: any;
  type?: string;
}

export enum Orientation {
  Horizontal = 'horizontal',
  Vertical = 'vertical',
}

export interface Preset {
  adjustments: Partial<Adjustments>;
  folder?: Folder;
  id: string;
  name: string;
  /**
   * Which adjustment groups the preset carries, by key. Absent on presets
   * saved before the full list existed, which recorded only the two booleans
   * below; those are kept so an old preset still reads correctly.
   */
  includedAdjustments?: Array<string>;
  includeMasks?: boolean;
  includeCropTransform?: boolean;
  presetType?: 'tool' | 'style';
}

export interface Progress {
  completed?: number;
  current?: number;
  total: number;
}

export interface SelectedImage {
  exif: any;
  group_id?: string | null;
  height: number;
  isRaw: boolean;
  isReady: boolean;
  metadata?: any;
  original_base64?: string;
  originalUrl: string | null;
  path: string;
  thumbnailUrl: string;
  width: number;
}

export interface SortCriteria {
  key: string;
  label?: string;
  order: string;
}

export interface SupportedTypes {
  nonRaw: Array<string>;
  raw: Array<string>;
}

export enum LibraryDisplayMode {
  Grid = 'grid',
  Cull = 'cull',
  List = 'list',
}

/**
 * BLITZRAW: the grid's minimum cell width, in pixels.
 *
 * Replaces the three fixed sizes below, which topped out at 320 and left a
 * large screen showing postage stamps. The old names are kept only to read a
 * settings file written before this, through LEGACY_THUMBNAIL_SIZES.
 */
export const THUMBNAIL_SIZE_MIN = 200;
export const THUMBNAIL_SIZE_MAX = 800;
export const THUMBNAIL_SIZE_STEP = 20;
export const THUMBNAIL_SIZE_DEFAULT = 240;
export const THUMBNAIL_SIZE_DEFAULT_ANDROID = 200;

export const LEGACY_THUMBNAIL_SIZES: { [key: string]: number } = {
  small: 200,
  medium: 240,
  large: 320,
};

export enum ThumbnailSize {
  Large = 'large',
  Medium = 'medium',
  Small = 'small',
}

export interface TransformState {
  positionX: number;
  positionY: number;
  scale: number;
}

export interface UiVisibility {
  filmstrip: boolean;
  leftPanel: boolean;
  rightPanel: boolean;
}

export interface WaveformData {
  blue: string;
  green: string;
  height: number;
  luma: string;
  red: string;
  rgb: string;
  parade: string;
  vectorscope: string;
  width: number;
}

export interface CullingSettings {
  similarityThreshold: number;
  blurThreshold: number;
  groupSimilar: boolean;
  filterBlurry: boolean;
}

export interface ImageAnalysisResult {
  path: string;
  qualityScore: number;
  sharpnessMetric: number;
  centerFocusMetric: number;
  exposureMetric: number;
  width: number;
  height: number;
}

export interface CullGroup {
  representative: ImageAnalysisResult;
  duplicates: ImageAnalysisResult[];
}

export interface CullingSuggestions {
  similarGroups: CullGroup[];
  blurryImages: ImageAnalysisResult[];
  failedPaths: string[];
}

export interface KeybindHandler {
  shouldFire?: () => boolean;
  execute: (event: KeyboardEvent) => void;
}

export type AlbumItem = Album | AlbumGroup;

export interface Album {
  type: 'album';
  id: string;
  name: string;
  icon?: string;
  images: string[];
}

export interface AlbumGroup {
  type: 'group';
  id: string;
  name: string;
  icon?: string;
  children: AlbumItem[];
}
