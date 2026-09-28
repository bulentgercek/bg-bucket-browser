import { beforeEach, describe, expect, it, vi } from "vitest";

// The backend is replaced: listings answer when the test says so, and every
// queued job is recorded with its arguments.
vi.mock("../lib/commands", async (original) => ({
  ...(await original<typeof import("../lib/commands")>()),
  listRemote: vi.fn(),
  listLocal: vi.fn(),
  transferStart: vi.fn(),
  devlogToast: vi.fn(),
  devlogVerbose: vi.fn(),
}));
const { listRemote, transferStart } = await import("../lib/commands");
const { runTransfer, isPreparingOn } = await import("./transferQueueStore");
const { useConnectionStore } = await import("./connectionStore");

const file = { name: "a.txt", kind: "file" as const, size: 1, modified: 0, glyph: null };

describe("runTransfer", () => {
  beforeEach(() => {
    vi.mocked(listRemote).mockReset();
    vi.mocked(transferStart).mockReset().mockResolvedValue("t1");
    useConnectionStore.setState({
      activeId: "A",
      connections: [
        { id: "A", name: "A", bucket: "a" },
        { id: "B", name: "B", bucket: "b" },
      ] as never,
    });
  });

  // The destination is listed before the job is queued, and a remote listing
  // takes seconds. Switching the connection meanwhile must not move the job.
  it("queues the job on the connection it was started with", async () => {
    let answer: (entries: never[]) => void = () => {};
    vi.mocked(listRemote).mockReturnValue(new Promise((resolve) => (answer = resolve)));

    const started = runTransfer({
      items: [file],
      srcSide: "local",
      srcPath: "~/x",
      destSide: "remote",
      destPath: "models",
      mode: "copy",
    });
    useConnectionStore.setState({ activeId: "B" });
    answer([]);
    await started;

    expect(vi.mocked(listRemote).mock.calls[0]).toEqual(["models", "A"]);
    expect(vi.mocked(transferStart)).toHaveBeenCalledTimes(1);
    expect(vi.mocked(transferStart).mock.calls[0].at(-1)).toBe("A");
  });

  // Until the job reaches the Rust queue, Settings learns of it only here.
  it("holds its connection busy while the job is being prepared", async () => {
    let answer: (entries: never[]) => void = () => {};
    vi.mocked(listRemote).mockReturnValue(new Promise((resolve) => (answer = resolve)));

    const started = runTransfer({
      items: [file],
      srcSide: "local",
      srcPath: "~/x",
      destSide: "remote",
      destPath: "models",
      mode: "copy",
    });
    expect(isPreparingOn("A")).toBe(true);
    answer([]);
    await started;
    expect(isPreparingOn("A")).toBe(false);
  });
});
