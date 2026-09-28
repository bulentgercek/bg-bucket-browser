/* The webview's own reload keys never apply here. WebView2 reloads the whole
   page on F5 and on control + R, which throws away the open window, the
   Settings form and the queue's rows while their jobs go on in the core.

   Only the default action is cancelled; the event goes on, so the app's own
   meanings for these keys (F5 copies, control + R refreshes the panes) still
   reach their listeners. The listener sits at the window in the capture phase
   and is added at startup: an open window stops keys at the same place, and
   stopping an event there does not hold back another listener on the window. */

/** True for the keys a browser reloads the page with. With a layout whose
    letters are not Latin, R still reloads by its place on the keyboard; a
    Latin layout that puts another letter there (Dvorak's P) keeps that key. */
export function isReloadKey(
  e: Pick<KeyboardEvent, "key" | "code" | "ctrlKey" | "metaKey" | "altKey">,
): boolean {
  if (e.key === "F5" || e.key === "BrowserRefresh") return true;
  return (
    (e.ctrlKey || e.metaKey) &&
    !e.altKey &&
    (e.key.toLowerCase() === "r" || (e.code === "KeyR" && !/^[a-z]$/i.test(e.key)))
  );
}

/** Cancels the reload keys wherever they are pressed; returns the function
    that stops listening. */
export function initReloadGuard(target: EventTarget = window): () => void {
  const onKey = (e: Event) => {
    if (isReloadKey(e as KeyboardEvent)) e.preventDefault();
  };
  // The options object rather than `true`: Node 24's EventTarget, which the
  // tests run on, does not remove a capture listener named by a boolean.
  target.addEventListener("keydown", onKey, { capture: true });
  return () => target.removeEventListener("keydown", onKey, { capture: true });
}
