import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// The native read is replaced, so a test decides when it answers.
vi.mock("../lib/commands", async (original) => ({
  ...(await original<typeof import("../lib/commands")>()),
  readOsClipboardFiles: vi.fn(),
}));
const { readOsClipboardFiles } = await import("../lib/commands");
const { useClipboardStore } = await import("./clipboardStore");

const empty = { paths: [], cut: false, known: true };

/** The answer a test is holding back; released after each test, so one that
    fails early does not leave the next waiting on it. */
let answer: (s: typeof empty) => void = () => {};
function holdAnswer() {
  vi.mocked(readOsClipboardFiles).mockReturnValue(new Promise((resolve) => (answer = resolve)));
}

describe("refreshOsClipboard", () => {
  beforeEach(() => {
    vi.mocked(readOsClipboardFiles).mockReset();
  });
  afterEach(async () => {
    answer(empty);
    await useClipboardStore.getState().refreshOsClipboard();
    vi.useRealTimers();
  });

  // The clipboard is polled every second. An owner that never answers must
  // hold one read, not gather a new one on every tick.
  it("skips a poll while the last read is still waiting", async () => {
    holdAnswer();

    const first = useClipboardStore.getState().refreshOsClipboard();
    void useClipboardStore.getState().refreshOsClipboard();
    void useClipboardStore.getState().refreshOsClipboard();
    expect(vi.mocked(readOsClipboardFiles)).toHaveBeenCalledTimes(1);

    answer(empty);
    await first;
    vi.mocked(readOsClipboardFiles).mockResolvedValue(empty);
    await useClipboardStore.getState().refreshOsClipboard();
    expect(vi.mocked(readOsClipboardFiles)).toHaveBeenCalledTimes(2);
  });

  // A row that can no longer be checked must not stay pasteable: meanwhile
  // something else may have been copied outside.
  it("takes the row down when a read does not answer", async () => {
    vi.useFakeTimers();
    useClipboardStore.setState({ osClipboardTag: { kind: "files", paths: ["/a"], mode: "move" } });
    holdAnswer();
    void useClipboardStore.getState().refreshOsClipboard();
    await vi.advanceTimersByTimeAsync(3000);
    expect(useClipboardStore.getState().osClipboardTag).toBeNull();
  });

  // A read that could not finish says nothing about the clipboard. Taking it
  // for "empty" made the next read look like a new copy from outside, which
  // then pushed the in-app copy off the slot.
  it("keeps the in-app copy when a read could not finish", async () => {
    const inApp = {
      items: [],
      srcSide: "remote" as const,
      srcPath: "models",
      mode: "copy" as const,
      origin: "inApp" as const,
    };
    useClipboardStore.setState({ clipboard: inApp, seenOsSignature: "/home/u/x.bin" });
    vi.mocked(readOsClipboardFiles).mockResolvedValue({ paths: [], cut: false, known: false });
    await useClipboardStore.getState().refreshOsClipboard();
    const s = useClipboardStore.getState();
    expect(s.clipboard).toEqual(inApp);
    expect(s.seenOsSignature).toBe("/home/u/x.bin");
    expect(s.osClipboardTag).toBeNull();
  });

  it("reads again after a read that failed", async () => {
    vi.mocked(readOsClipboardFiles).mockRejectedValueOnce(new Error("no clipboard"));
    await useClipboardStore.getState().refreshOsClipboard();
    vi.mocked(readOsClipboardFiles).mockResolvedValue(empty);
    await useClipboardStore.getState().refreshOsClipboard();
    expect(vi.mocked(readOsClipboardFiles)).toHaveBeenCalledTimes(2);
  });
});
