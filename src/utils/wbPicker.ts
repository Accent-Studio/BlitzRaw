import { useEditorStore } from '../store/useEditorStore';

/**
 * BLITZRAW: turns the white balance eyedropper off.
 *
 * The eyedropper is a mode, and it used to be a mode with no way out except
 * pressing the same button again. Nothing ended it: `handleWbPicked` was an
 * empty function, so it survived every pick and every edit made afterwards. A
 * mode you cannot see is a mode you forget you are in, and the next click on
 * the photo sets a white balance nobody asked for.
 *
 * **Ending it on the pick would be wrong**, and was tried. Finding a neutral
 * means clicking several places in turn and looking at each result, so a
 * one-shot tool has to be picked up again between every try. The pick is the
 * middle of the job, not the end of it.
 *
 * What ends it is **the pointer leaving the photo**. That is where somebody has
 * finished with it, it is the same moment they stop being able to see that the
 * tool is still in their hand, and outside the photo a click did nothing
 * anyway: `handleWbClick` rejects a pick outside the image bounds, and the
 * check that lets go uses those same bounds. Touching any slider ends it too,
 * which is the same person moving on by a different route.
 *
 * Reads the store rather than taking a setter, so any control can call it
 * without being wired for it, and does nothing when the tool is already down.
 */
export function dismissWbPicker(): void {
  const { isWbPickerActive, setEditor } = useEditorStore.getState();
  if (isWbPickerActive) {
    setEditor({ isWbPickerActive: false });
  }
}
