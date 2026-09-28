import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

// The core's events are caught here, so a test can send one itself.
const handlers = new Map<string, (e: { payload: unknown }) => void>();
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn((name: string, cb: (e: { payload: unknown }) => void) => {
    handlers.set(name, cb);
    return Promise.resolve(() => {});
  }),
}));
vi.mock("../lib/commands", async (original) => ({
  ...(await original<typeof import("../lib/commands")>()),
  devlogToast: vi.fn(),
  devlogVerbose: vi.fn(),
}));
const { initTransferEvents, useTransferStore } = await import("./transferQueueStore");
const { useToastStore } = await import("./toastStore");

/** The handler the store registered for an event; a missing one fails the test. */
function handler(name: string) {
  const h = handlers.get(name);
  if (!h) throw new Error(`no listener for ${name}`);
  return h;
}

describe("a job's error", () => {
  // The store wires its listeners once per run.
  beforeAll(async () => {
    await initTransferEvents();
  });
  beforeEach(() => {
    useTransferStore.setState({ transfers: [] });
    useToastStore.setState({ toasts: [] });
  });

  // A job refused at once can end before the answer that adds its row, so the
  // error carries the job's name itself.
  it("names the job even when it ended before its row appeared", () => {
    handler("transfer-error")({
      payload: { id: "t4", name: "CON", detail: "not transferred: the name is not a safe file name on this system" },
    });
    expect(useToastStore.getState().toasts.map((t) => t.text)).toEqual([
      "CON: not transferred: the name is not a safe file name on this system",
    ]);
  });
});
