import { describe, expect, it } from "vitest";
import { initReloadGuard, isReloadKey } from "./reloadGuard";

type Mods = { ctrlKey?: boolean; metaKey?: boolean; altKey?: boolean; shiftKey?: boolean; code?: string };

/** A cancelable keydown; the test environment has no KeyboardEvent. */
function keydown(key: string, mods: Mods = {}) {
  return Object.assign(new Event("keydown", { cancelable: true }), {
    key,
    code: "",
    ctrlKey: false,
    metaKey: false,
    altKey: false,
    shiftKey: false,
    ...mods,
  });
}

describe("the reload guard", () => {
  it("knows the reload keys and nothing else", () => {
    for (const [key, mods] of [
      ["F5", {}],
      ["F5", { shiftKey: true }],
      ["F5", { ctrlKey: true }],
      ["BrowserRefresh", {}],
      ["r", { ctrlKey: true }],
      ["R", { ctrlKey: true, shiftKey: true }],
      ["r", { metaKey: true }],
      ["к", { ctrlKey: true, code: "KeyR" }],
    ] as [string, Mods][]) {
      expect(isReloadKey(keydown(key, mods)), `${key} ${JSON.stringify(mods)}`).toBe(true);
    }
    for (const [key, mods] of [
      ["r", {}],
      ["F6", {}],
      ["r", { ctrlKey: true, altKey: true }],
      ["c", { ctrlKey: true }],
      ["p", { ctrlKey: true, code: "KeyR" }],
    ] as [string, Mods][]) {
      expect(isReloadKey(keydown(key, mods)), `${key} ${JSON.stringify(mods)}`).toBe(false);
    }
  });

  // An open window catches every key before the panes and stops it, so the
  // cancel has to happen in a listener of its own.
  it("cancels F5 and control + R, and the app's own listeners still see them", () => {
    const win = new EventTarget();
    const stop = initReloadGuard(win);
    const seen: string[] = [];
    win.addEventListener("keydown", (e) => seen.push((e as KeyboardEvent).key), true);

    const f5 = keydown("F5");
    win.dispatchEvent(f5);
    expect(f5.defaultPrevented).toBe(true);

    const ctrlR = keydown("r", { ctrlKey: true });
    win.dispatchEvent(ctrlR);
    expect(ctrlR.defaultPrevented).toBe(true);

    const typing = keydown("r");
    win.dispatchEvent(typing);
    expect(typing.defaultPrevented).toBe(false);

    expect(seen).toEqual(["F5", "r", "r"]);

    stop();
    const after = keydown("F5");
    win.dispatchEvent(after);
    expect(after.defaultPrevented).toBe(false);
  });

  // The open window's listener comes first and stops the key, as Dialog's does;
  // stopping an event does not hold back another listener on the same target.
  it("still cancels F5 when a listener before it stops the event", () => {
    const win = new EventTarget();
    win.addEventListener("keydown", (e) => e.stopPropagation(), true);
    const stop = initReloadGuard(win);
    const f5 = keydown("F5");
    win.dispatchEvent(f5);
    stop();
    expect(f5.defaultPrevented).toBe(true);
  });
});
