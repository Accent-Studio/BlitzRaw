import { create } from 'zustand';
import { recordAppAction } from '../utils/appHistory';
import {
  FilterCriteria,
  ImageFile,
  RawStatus,
  SortCriteria,
  SortDirection,
  AlbumItem,
} from '../components/ui/AppProperties';
import { Adjustments, INITIAL_ADJUSTMENTS } from '../utils/adjustments';
import { ColumnWidths } from '../components/panel/MainLibrary';

export interface SearchCriteria {
  tags: string[];
  text: string;
  mode: 'AND' | 'OR';
}

/**
 * BLITZRAW: one rating change, small enough to keep a lot of.
 *
 * Only the photos the change touched appear, with what they were rated and
 * what they became. Storing the whole ratings map per step would be a copy of
 * every photo in the folder per keystroke, and a cull is a lot of keystrokes.
 */
export interface RatingChange {
  before: Record<string, number>;
  after: Record<string, number>;
}

/**
 * How many rating changes are worth keeping. Deep enough to walk back out of a
 * wrong turn during a cull, shallow enough that nobody is relying on it as a
 * record of what they did.
 */
export const RATING_HISTORY_LIMIT = 100;

/** Numbers the rating actions in the application's own list. */
let ratingActions = 0;

interface LibraryState {
  // Paths & Trees
  rootPaths: string[];
  currentFolderPath: string | null;
  expandedFolders: Set<string>;
  folderTrees: any[];
  pinnedFolderTrees: any[];

  // Albums
  albumTree: AlbumItem[];
  activeAlbumId: string | null;
  expandedAlbumGroups: Set<string>;

  // Images & Selection
  imageList: Array<ImageFile>;
  imageRatings: Record<string, number>;
  /** Which RAW files carry a compression no open source decoder can read.
   *  Keyed by path; absent means not yet probed. */
  rawCompression: Record<string, { label: string | null; needsConversion: boolean }>;
  multiSelectedPaths: Array<string>;
  /** Stack ids the user has opened. Deliberately per-session: reopening the
   *  folder shows stacks closed again, which is the point of stacking. */
  expandedStacks: Array<string>;
  /**
   * The frame the pointer is over, in the grid or the strip.
   *
   * Separate from the selection on purpose: hovering is a look, not a
   * choice, and nothing acts on it. Only the navigator reads it.
   */
  hoveredPath: string | null;
  selectionAnchorPath: string | null;
  libraryActivePath: string | null;
  libraryActiveAdjustments: Adjustments;

  // Sorting & Filtering
  sortCriteria: SortCriteria;
  filterCriteria: FilterCriteria;
  searchCriteria: SearchCriteria;

  // UI State specific to the Library View
  isTreeLoading: boolean;
  isViewLoading: boolean;
  libraryScrollTop: number;
  listColumnWidths: ColumnWidths;

  // ============ BLITZRAW: bring a frame into view without selecting it ============
  /**
   * A request for the grid to scroll a frame into view, and nothing else.
   *
   * The grid already scrolls to the current frame, but it stops doing so the
   * moment more than one photo is selected, which is exactly when growing a
   * selection with Alt and an arrow needs it. Moving the current frame to the
   * growing end would scroll for free, and was tried: it read as the selection
   * dragging you around, because the frame the panel is editing kept walking
   * away from you.
   *
   * So the two are separated. This says where to look; `libraryActivePath`
   * still says what is being worked on, and they are allowed to differ.
   *
   * `id` rises on every request, because the same frame can be asked for twice
   * running: reach out, come back, reach out again lands on the same path and
   * a bare string would not look like a new request.
   *
   * `center` puts the frame in the middle of the view at once, rather than
   * sliding it just far enough in. Reopening a session uses it: the photo can
   * be thousands of rows down, and a smooth scroll of that length is a long
   * swoop past everything else before the app has finished starting.
   */
  scrollRequest: { path: string; id: number; center?: boolean } | null;
  // ========== BLITZRAW END: bring a frame into view without selecting it ==========

  // ============ BLITZRAW: ratings can be taken back ============
  /**
   * Rating changes, oldest first, each one a before and after picture of only
   * the photos it touched.
   *
   * Ratings never went through the editor's undo. That history is a list of
   * whole adjustment states for one open photo, and a rating is neither: it
   * belongs to the library, it usually lands on a selection rather than one
   * file, and in the grid there is no open photo to have a history at all. So
   * Ctrl+Z in the grid did nothing, and a star pressed by accident had to be
   * remembered and undone by hand.
   *
   * Kept in memory for the session only. It is a way back out of the last few
   * keystrokes, not a record; the sidecars are the record.
   */
  ratingHistory: Array<RatingChange>;
  /** Undone rating changes, newest first, waiting for a redo. */
  ratingFuture: Array<RatingChange>;
  /**
   * When the rating stacks last moved, undo included. Ctrl+Z compares this
   * against the editor's `historyChangedAt` and takes the more recent of the
   * two, so undo works on whichever thing was actually done last.
   */
  ratingChangedAt: number | null;
  // ========== BLITZRAW END: ratings can be taken back ==========

  // Actions
  setLibrary: (updater: Partial<LibraryState> | ((state: LibraryState) => Partial<LibraryState>)) => void;
  /** Records a rating change so it can be taken back. */
  pushRatingChange: (change: RatingChange) => void;
  undoRating: () => RatingChange | null;
  redoRating: () => RatingChange | null;
  clearSelection: () => void;
  setFilterCriteria: (criteria: Partial<FilterCriteria> | ((prev: FilterCriteria) => FilterCriteria)) => void;
  setSearchCriteria: (criteria: Partial<SearchCriteria> | ((prev: SearchCriteria) => SearchCriteria)) => void;
  setSortCriteria: (criteria: Partial<SortCriteria> | ((prev: SortCriteria) => SortCriteria)) => void;
}

export const useLibraryStore = create<LibraryState>((set, get) => ({
  rootPaths: [],
  currentFolderPath: null,
  expandedStacks: [],
  hoveredPath: null,
  expandedFolders: new Set<string>(),
  folderTrees: [],
  pinnedFolderTrees: [],

  albumTree: [],
  activeAlbumId: null,
  expandedAlbumGroups: new Set<string>(),

  imageList: [],
  imageRatings: {},
  rawCompression: {},
  multiSelectedPaths: [],
  selectionAnchorPath: null,
  libraryActivePath: null,
  libraryActiveAdjustments: INITIAL_ADJUSTMENTS,

  sortCriteria: { key: 'name', order: SortDirection.Ascending },
  filterCriteria: { colors: [], rating: 0, rawStatus: RawStatus.All },
  searchCriteria: { tags: [], text: '', mode: 'OR' },

  ratingHistory: [],
  ratingFuture: [],
  ratingChangedAt: null,

  isTreeLoading: false,
  isViewLoading: false,
  libraryScrollTop: 0,
  scrollRequest: null,
  listColumnWidths: {
    thumbnail: 4,
    name: 20,
    date: 15,
    rating: 8,
    color: 8,
    shutter: 10,
    aperture: 10,
    iso: 10,
    focal: 15,
  },

  setLibrary: (updater) => set((state) => (typeof updater === 'function' ? updater(state) : updater)),

  clearSelection: () => set({ multiSelectedPaths: [], libraryActivePath: null }),

  // ============ BLITZRAW: ratings can be taken back ============
  pushRatingChange: (change) => {
    // BLITZRAW: and into the one list of what I did, so Ctrl+Z behaves the same
    // whether the last thing I did was a rating or an edit.
    //
    // A rating is the one kind of action the app list has to hold values for.
    // Every other kind is written into the photos, key by key, with the value
    // before and after; ratings are not part of a photo's edit history and
    // never should be, so this stack is the only record there is.
    recordAppAction({
      id: `r${(ratingActions += 1)}`,
      kind: 'ratings',
      label: 'Rating',
      // A rating is not a step in a photo's edit history and never should be,
      // so it has no numbers to move between. Its own stack is the record, and
      // this list only says when it happened relative to everything else.
      photos: Object.keys(change.after).map((path) => ({ path, from: 0, to: 0 })),
      selection: Object.keys(change.after),
      openPath: null,
      inEditor: false,
    });
    set((state) => ({
      // Bounded, because this is a way out of the last few keystrokes rather
      // than a record of the session, and a cull is thousands of keystrokes.
      ratingHistory: [...state.ratingHistory, change].slice(-RATING_HISTORY_LIMIT),
      // A new change is a new branch. Anything that was waiting to be redone
      // belongs to a path nobody took.
      ratingFuture: [],
      ratingChangedAt: Date.now(),
    }));
  },

  // Reads through zustand's own `get` rather than the exported store. Reaching
  // for the const while it is still being defined makes its type circular, and
  // TypeScript answers by giving the whole store `any`.
  undoRating: () => {
    const change = get().ratingHistory.at(-1) ?? null;
    if (!change) {
      return null;
    }
    set((state) => ({
      imageRatings: { ...state.imageRatings, ...change.before },
      ratingHistory: state.ratingHistory.slice(0, -1),
      ratingFuture: [change, ...state.ratingFuture],
      ratingChangedAt: Date.now(),
    }));
    return change;
  },

  redoRating: () => {
    const change = get().ratingFuture[0] ?? null;
    if (!change) {
      return null;
    }
    set((state) => ({
      imageRatings: { ...state.imageRatings, ...change.after },
      ratingHistory: [...state.ratingHistory, change],
      ratingFuture: state.ratingFuture.slice(1),
      ratingChangedAt: Date.now(),
    }));
    return change;
  },
  // ========== BLITZRAW END: ratings can be taken back ==========

  setFilterCriteria: (criteria) =>
    set((state) => ({
      filterCriteria:
        typeof criteria === 'function' ? criteria(state.filterCriteria) : { ...state.filterCriteria, ...criteria },
    })),

  setSearchCriteria: (criteria) =>
    set((state) => ({
      searchCriteria:
        typeof criteria === 'function' ? criteria(state.searchCriteria) : { ...state.searchCriteria, ...criteria },
    })),

  setSortCriteria: (criteria) =>
    set((state) => ({
      sortCriteria:
        typeof criteria === 'function' ? criteria(state.sortCriteria) : { ...state.sortCriteria, ...criteria },
    })),
}));
