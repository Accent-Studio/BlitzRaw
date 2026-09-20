import { create } from 'zustand';
import { Adjustments, INITIAL_ADJUSTMENTS, MaskContainer, AiPatch } from '../utils/adjustments';
import { SelectedImage, WaveformData, BrushSettings } from '../components/ui/AppProperties';
import { ChannelConfig } from '../components/adjustments/Curves';
import { ImageDimensions } from '../hooks/useImageRenderSize';
import { ToolType } from '../components/panel/right/Masks';
import { OverlayMode } from '../components/panel/right/CropPanel';
import {
  DEFAULT_HISTORY_STEPS,
  appendStep,
  historyOnOpen,
  rememberHistory,
  historyFromLog,
  type RememberedHistory,
  type StoredHistory,
} from '../utils/editHistory';

export interface InteractivePatch {
  url: string;
  normX: number;
  normY: number;
  normW: number;
  normH: number;
}

interface BaseRenderSize extends ImageDimensions {
  containerHeight: number;
  containerWidth: number;
  offsetX: number;
  offsetY: number;
}

interface EditorState {
  // Core Image & Adjustments
  selectedImage: SelectedImage | null;
  adjustments: Adjustments;
  /**
   * BLITZRAW: which photo the adjustments above actually describe.
   *
   * Opening a photo sets `selectedImage` at once and its adjustments a moment
   * later, when the sidecar has been read. In between, the store is still
   * holding the last photo's numbers, and there is no way to tell that from the
   * numbers themselves. Anything that wants to change an adjustment before the
   * photo has finished opening has to know the difference, or it nudges one
   * photo's exposure from another photo's starting point.
   *
   * Set by `resetHistory`, which is what every load calls once it has the real
   * values. Null when nothing is open.
   */
  adjustmentsPath: string | null;
  /**
   * BLITZRAW: the adjustments this photo was opened with.
   *
   * The picture in `.blitzraw-previews` is the photo as it was adjusted when
   * that file was written, which is where it stood when it opened. So this is
   * the base a nudge is measured from while the raw decodes. See
   * utils/proxyPreview.ts.
   *
   * Read off the sidecar rather than the store, because the sidecar is written
   * on a delay and a held key would have it part-way through the run.
   */
  adjustmentsAtOpen: Adjustments | null;
  previewOverride: Adjustments | null;

  // History State
  history: Adjustments[];
  /** A name per step, or null where the toolbar should work one out. */
  historyLabels: Array<string | null>;
  /**
   * BLITZRAW: each step's number in the photo's own file, or -1 when a step was
   * made this session and the write has not come back yet.
   *
   * What lets a click in the History panel be recorded in the list of what I
   * did, and taken back like anything else.
   */
  historyNumbers: Array<number>;
  /**
   * BLITZRAW: which photo the scopes on screen describe.
   *
   * They used to be able to describe only whatever was open, so there was
   * nothing to say. They now follow the pointer the way the navigator does,
   * which means knowing whose they are.
   */
  scopesPath: string | null;
  /**
   * BLITZRAW: the last scopes the render pipeline produced, and for what.
   *
   * Kept apart from the ones on screen so that unhovering can put them back.
   * They are the accurate pair, measured on the full render rather than on a
   * preview, and losing them to a glance at the next frame would mean the photo
   * being worked on quietly dropped to preview quality until a slider moved.
   */
  editorScopes: { path: string; histogram: any; waveform: any } | null;
  /** When the newest step was written, for joining a run of nudges to it. */
  historyLastStepAt: number | null;
  /**
   * BLITZRAW: when the adjustment history last moved, undo included.
   *
   * Ctrl+Z has two stacks to choose between now: this one and the rating one in
   * the library store. The rule is that the more recent of the two goes first,
   * and that undoing keeps you in the stack you started in until it runs out,
   * which is why an undo stamps this as well as a step does. Unlike
   * `historyLastStepAt`, which is about coalescing a run of slider moves into
   * one step and is deliberately cleared when you move through the history.
   */
  historyChangedAt: number | null;
  /**
   * BLITZRAW: when the adjustments last arrived from walking the history.
   *
   * Stamped by undo, redo and a click in the history list, and by nothing else.
   * `historyChangedAt` cannot answer this: an ordinary edit stamps it too, and
   * a click in the list used to stamp nothing at all.
   *
   * What needs to know is auto-sync. Undoing a change that went out to a
   * selection would otherwise send the undone state out to that selection
   * again, so recovering from a mistake repeats it. See utils/autoSync.ts.
   */
  historyMoveAt: number | null;
  /** Whether the open photo's history came from its sidecar rather than from
   * this session. One that did needs no copy kept in memory when it is left. */
  historyFromDisk: boolean;
  /**
   * A name for the step the next save should carry.
   *
   * Set when an action names itself, like Reset, and consumed by the save. It
   * cannot be sent at the moment it happens because the save is rate-limited
   * and lands a fraction of a second later.
   */
  historyPendingLabel: string | null;
  historyIndex: number;
  /**
   * Which photo the live history belongs to, so it can be put aside under the
   * right key when another one is opened. Null before anything is open.
   */
  historyKey: string | null;
  /** One history per photo, most recently opened first. See editHistory.ts. */
  rememberedHistories: Array<RememberedHistory>;
  /** How many steps one photo keeps. A setting, because it is a memory cost. */
  historyStepLimit: number;

  // Previews & Overlays
  finalPreviewUrl: string | null;
  uncroppedAdjustedPreviewUrl: string | null;
  transformedOriginalUrl: string | null;
  interactivePatch: InteractivePatch | null;
  showOriginal: boolean;

  // Analytics
  histogram: ChannelConfig | null;
  waveform: WaveformData | null;
  isWaveformVisible: boolean;
  activeWaveformChannel: string;
  /**
   * Which scopes the Scopes panel is showing, top to bottom.
   *
   * A list because the panel stacks them, and one list rather than one
   * flag each because the backend walks the pixels once for whatever is
   * asked for: three scopes cost one pass, not three.
   */
  waveformChannels: Array<string>;
  /**
   * How far the vectorscope's trace is pushed out from the centre. One is the
   * scope as it has always been drawn. The graticule does not move with it, or
   * the reading would mean nothing. Resolve calls the same control gain.
   */
  vectorscopeGain: number;
  /**
   * The height of each scope in the column, in the column's order. Shorter or
   * longer than the column is normal, since the column is changed by the plus
   * and the minus; `scopeHeightsFor` fits one to the other.
   */
  scopeHeights: Array<number>;
  waveformHeight: number;

  /**
   * BLITZRAW: the preview built for this photo on disk, if it has one.
   *
   * Shown in place of the thumbnail while the decode runs, which is the second
   * and a half the preview cache exists to hide. Not the render: it is replaced
   * by finalPreviewUrl the moment that arrives, and nothing samples pixels from
   * it.
   */
  cachedPreviewUrl: string | null;

  // Interaction State
  isSliderDragging: boolean;
  /**
   * BLITZRAW: a number is being typed into a slider's field.
   *
   * Typing arrives one character at a time, so 4800 comes through as 4, then
   * 48, then 480. Anything that fans an edit out across a selection waits for
   * this to clear, the way it already waits for a drag to end.
   */
  isSliderTyping: boolean;
  zoom: number;
  displaySize: ImageDimensions;
  previewSize: ImageDimensions;
  baseRenderSize: BaseRenderSize;
  originalSize: ImageDimensions;

  // Tools State
  isRotationActive: boolean;
  overlayMode: OverlayMode;
  overlayRotation: number;
  isStraightenActive: boolean;
  isWbPickerActive: boolean;
  liveRotation: number | null;
  brushSettings: BrushSettings | null;

  // Masks & AI
  activeMaskContainerId: string | null;
  activeMaskId: string | null;
  activeAiPatchContainerId: string | null;
  activeAiSubMaskId: string | null;
  isMaskControlHovered: boolean;
  /**
   * BLITZRAW: which mask the pointer is resting on in the list at the top.
   *
   * The one gesture that means "show me where this one is", and the only thing
   * that brings the red overlay back once a mask has been given adjustments.
   * Null when the pointer is on none of them. See utils/maskOverlay.ts.
   */
  hoveredMaskContainerId: string | null;
  isGeneratingAiMask: boolean;
  isGeneratingAi: boolean;
  isAIConnectorConnected: boolean;
  hasRenderedFirstFrame: boolean;
  /**
   * BLITZRAW: whether the backend has this photo decoded and in hand.
   *
   * Not the same as `selectedImage.isReady`, which only says the front end has
   * something to draw. Opening a photo it has seen before fills the editor from
   * its own cache at once and asks the backend for the real decode in the
   * background, so for a second or two the front end is ready and the backend
   * has nothing. Anything that asks the backend to render in that window is
   * answered with "No original image loaded" and simply does not happen: that
   * is the crop rectangle failing to appear when the crop panel is opened
   * straight from the grid.
   *
   * A ref carried this already, and a ref cannot wake an effect when it
   * changes, which is why nothing ever asked again.
   */
  isBackendReady: boolean;
  patchesSentToBackend: Set<string>;

  // Clipboard
  copiedSectionAdjustments: any | null;
  copiedMask: MaskContainer | null;
  copiedAdjustments: Adjustments | null;

  // Actions
  setEditor: (updater: Partial<EditorState> | ((state: EditorState) => Partial<EditorState>)) => void;
  /**
   * Adds a step, or extends the run at the top. See `appendStep`.
   *
   * `forPath` is the photo the change was made to. A push that arrives after a
   * different photo has been opened is dropped rather than written into the
   * wrong history.
   */
  pushHistory: (newAdjustments: Adjustments, label?: string | null, forPath?: string | null) => void;
  undo: () => void;
  redo: () => void;
  /**
   * Picks up the history for whatever photo is now open.
   *
   * `storedLog` is what its sidecar holds, and it is authoritative when there
   * is one: it outlives the session and it has the steps taken while the photo
   * was closed, which nothing in memory can know about.
   */
  resetHistory: (initialState: Adjustments, storedLog?: StoredHistory | null) => void;
  goToHistoryIndex: (index: number) => void;
  /** BLITZRAW: records the number the backend gave the step just written. */
  numberCurrentStep: (path: string, n: number) => void;
}

export const useEditorStore = create<EditorState>((set) => ({
  selectedImage: null,
  adjustments: INITIAL_ADJUSTMENTS,
  previewOverride: null,
  history: [INITIAL_ADJUSTMENTS],
  historyLabels: [null],
  historyNumbers: [-1],
  scopesPath: null,
  editorScopes: null,
  historyLastStepAt: null,
  historyChangedAt: null,
  historyMoveAt: null,
  historyFromDisk: false,
  historyPendingLabel: null,
  historyIndex: 0,
  historyKey: null,
  rememberedHistories: [],
  historyStepLimit: DEFAULT_HISTORY_STEPS,

  finalPreviewUrl: null,
  uncroppedAdjustedPreviewUrl: null,
  showOriginal: false,
  histogram: null,
  waveform: null,
  isWaveformVisible: false,
  activeWaveformChannel: 'luma',
  waveformChannels: ['luma'],
  vectorscopeGain: 1,
  scopeHeights: [],
  waveformHeight: 220,

  cachedPreviewUrl: null,
  isSliderDragging: false,
  isSliderTyping: false,
  interactivePatch: null,
  activeMaskContainerId: null,
  activeMaskId: null,
  activeAiPatchContainerId: null,
  activeAiSubMaskId: null,

  zoom: 1,
  displaySize: { width: 0, height: 0 },
  previewSize: { width: 0, height: 0 },
  baseRenderSize: { width: 0, height: 0, offsetX: 0, offsetY: 0, containerWidth: 0, containerHeight: 0 },
  originalSize: { width: 0, height: 0 },

  isRotationActive: false,
  overlayMode: 'thirds',
  overlayRotation: 0,
  transformedOriginalUrl: null,
  isStraightenActive: false,
  isWbPickerActive: false,
  liveRotation: null,

  copiedSectionAdjustments: null,
  copiedMask: null,
  brushSettings: { size: 50, feather: 50, tool: ToolType.Brush },
  copiedAdjustments: null,

  isGeneratingAiMask: false,
  isAIConnectorConnected: false,
  isGeneratingAi: false,
  isMaskControlHovered: false,
  hoveredMaskContainerId: null,
  hasRenderedFirstFrame: false,
  isBackendReady: true,
  patchesSentToBackend: new Set<string>(),
  adjustmentsPath: null,
  adjustmentsAtOpen: null,

  setEditor: (updater) => set((state) => (typeof updater === 'function' ? updater(state) : updater)),

  /**
   * Adds a step. The only way anything enters a history.
   *
   * Used by the sliders, and by everything else that changes a photo: resetting
   * it, pasting a preset, applying an adjustment across a selection. Those are
   * things that happened and the way back out of them is the point.
   */
  pushHistory: (newAdj, label = null, forPath = null) =>
    set((state) => {
      // A push is rate-limited, so one can still be in the air when a different
      // photo is opened. Writing it into whatever is open now would put one
      // photo's move into another photo's history, which is a data bug rather
      // than a display one. Dropping it loses nothing: the adjustment itself
      // saves by its own path, and `historyOnOpen` notices the difference and
      // records a step the next time that photo is opened.
      if (forPath !== null && forPath !== (state.selectedImage?.path ?? null)) {
        return state;
      }
      const stepped = appendStep(
        state.history,
        state.historyLabels,
        state.historyIndex,
        newAdj,
        {
          label,
          limit: state.historyStepLimit,
          lastStepAt: state.historyLastStepAt,
        },
        state.historyNumbers,
      );
      return {
        history: stepped.entries,
        historyLabels: stepped.labels,
        historyNumbers: stepped.numbers,
        historyIndex: stepped.index,
        historyLastStepAt: stepped.lastStepAt,
        historyChangedAt: Date.now(),
        // Carried to the save, which is what puts it in the sidecar.
        historyPendingLabel: label ?? state.historyPendingLabel,
        // The first edit is what makes a history worth keeping, and it is also
        // the moment the key can be trusted: navigation has finished by the
        // time anybody has moved a slider.
        historyKey: state.historyKey ?? state.selectedImage?.path ?? null,
      };
    }),

  // ============ BLITZRAW: the number a step was given ============
  // A step made in this session has no number until the write comes back from
  // the backend saying which one it got. Filled in here, against the photo it
  // belongs to, so a click in the History panel can name a real step.
  numberCurrentStep: (path, n) =>
    set((state) => {
      if (state.historyKey !== path && state.selectedImage?.path !== path) {
        return state;
      }
      const numbers = [...state.historyNumbers];
      while (numbers.length < state.history.length) numbers.push(-1);
      numbers[state.historyIndex] = n;
      return { historyNumbers: numbers };
    }),
  // ========== BLITZRAW END: the number a step was given ==========

  undo: () =>
    set((state) => {
      if (state.historyIndex > 0) {
        const newIndex = state.historyIndex - 1;
        // Moving through a history ends whatever run was being made, the same
        // way leaving a photo does.
        return {
          historyIndex: newIndex,
          adjustments: state.history[newIndex],
          historyLastStepAt: null,
          historyChangedAt: Date.now(),
          historyMoveAt: Date.now(),
        };
      }
      return state;
    }),

  redo: () =>
    set((state) => {
      if (state.historyIndex < state.history.length - 1) {
        const newIndex = state.historyIndex + 1;
        return {
          historyIndex: newIndex,
          adjustments: state.history[newIndex],
          historyLastStepAt: null,
          historyChangedAt: Date.now(),
          historyMoveAt: Date.now(),
        };
      }
      return state;
    }),

  /**
   * Picks up the history for whatever photo is now open.
   *
   * Called on navigation. The history of the photo being left is put aside
   * under its own key, and the photo being opened gets its own back.
   *
   * **Nothing is thrown away here.** If the photo turns up in a state its
   * history does not lead to, something changed it while it was closed, most
   * likely a preset or an adjustment applied across a selection. That is a step
   * like any other and is appended, so the way back is still there. An earlier
   * version dropped the history in that case, which is exactly backwards: a
   * photo in an unexpected state does not mean the history is worthless, it
   * means something happened that the history has not recorded yet.
   */
  resetHistory: (initialState, storedLog = null) =>
    set((state) => {
      const leaving = state.historyKey;
      const arriving = state.selectedImage?.path ?? null;
      // Only what has nowhere else to live. A photo whose history came from
      // its sidecar has it on disk already, and holding a second copy of every
      // photo a long session touches is how a folder of 1,377 frames turns
      // into hundreds of megabytes of duplicate.
      const remembered = state.historyFromDisk
        ? state.rememberedHistories
        : rememberHistory(
            state.rememberedHistories,
            leaving,
            state.history,
            state.historyLabels,
            state.historyIndex,
            state.historyNumbers,
          );
      // Named, because a photo that changed while it was closed changed because
      // something was applied to a selection it was in, and the alternative
      // name is the list of forty things that moved.
      // The sidecar wins when there is one. Nothing in memory can know what
      // happened to a photo while it was closed; the log can, because whoever
      // wrote the change wrote the step.
      const opened = storedLog
        ? historyFromLog(storedLog, initialState, state.historyStepLimit)
        : historyOnOpen(remembered, arriving, initialState, state.historyStepLimit, 'Applied elsewhere');

      return {
        rememberedHistories: remembered,
        historyKey: arriving,
        // The values below are this photo's own, which is the whole point of
        // this call. See `adjustmentsPath`.
        adjustmentsPath: arriving,
        // And where it stood on arriving, which is the base a nudge made
        // during the decode is measured from. See `adjustmentsAtOpen`.
        adjustmentsAtOpen: initialState,
        history: opened.entries,
        historyLabels: opened.labels,
        historyNumbers: opened.numbers,
        historyIndex: opened.index,
        // Closed on arrival, so nothing joins a step made before the photo was
        // opened.
        historyLastStepAt: opened.lastStepAt,
        historyFromDisk: !!storedLog,
        adjustments: initialState,
      };
    }),

  goToHistoryIndex: (index) =>
    set((state) => {
      if (index >= 0 && index < state.history.length) {
        return {
          historyIndex: index,
          adjustments: state.history[index],
          historyLastStepAt: null,
          // Stamped here too. Clicking a row in the history list is the same
          // act as pressing undo several times, and it used to stamp nothing.
          historyChangedAt: Date.now(),
          historyMoveAt: Date.now(),
        };
      }
      return state;
    }),
}));
