import { beforeEach, describe, expect, it, vi } from "vitest";

// The backend is replaced; each test sees what the deletion was asked for.
vi.mock("../lib/commands", async (original) => ({
  ...(await original<typeof import("../lib/commands")>()),
  deleteScanned: vi.fn(),
  devlogToast: vi.fn(),
  devlogVerbose: vi.fn(),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
const { deleteScanned } = await import("../lib/commands");
const { useCleanupStore } = await import("./cleanupStore");

const entry = (key: string, etag: string) => ({ key, size: 1, etag });

describe("runDelete", () => {
  beforeEach(() => {
    vi.mocked(deleteScanned).mockReset().mockResolvedValue({ deleted: 2, changed: 0, failed: 0 });
  });

  // Each pick goes with the version the scan saw, from either list.
  it("sends each pick with its scanned version", async () => {
    useCleanupStore.setState({
      status: "done",
      report: {
        totalObjects: 2,
        totalBytes: 2,
        topFolders: [],
        largest: [entry("big.bin", '"v-big"')],
        reclaimable: [entry("x/__pycache__/a.pyc", '"v-pyc"')],
        reclaimableCount: 1,
        reclaimableBytes: 1,
        skipped: [],
        skippedCount: 0,
      },
      selected: new Set(["big.bin", "x/__pycache__/a.pyc"]),
    });
    await useCleanupStore.getState().runDelete();
    expect(vi.mocked(deleteScanned).mock.calls[0][0]).toEqual([
      { key: "big.bin", etag: '"v-big"' },
      { key: "x/__pycache__/a.pyc", etag: '"v-pyc"' },
    ]);
  });
});
