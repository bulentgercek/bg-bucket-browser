import { beforeEach, describe, expect, it, vi } from "vitest";

// The backend is replaced; the test sees every call that reaches it.
const rawInvoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke: rawInvoke }));
const { feedbackRecordStart, feedbackRecordStop, feedbackRecording } = await import("./commands");

/** What the wrapper wrote to the verbose log, one line per entry. */
const logged = () =>
  rawInvoke.mock.calls
    .filter(([cmd]) => cmd === "devlog_verbose")
    .map(([, args]) => (args as { msg: string }).msg);

describe("the command log", () => {
  beforeEach(() => {
    rawInvoke.mockReset().mockResolvedValue(null);
  });

  // Their lines would be the first and last thing the user reads in the log
  // attached to a report; the backend marks both moments itself.
  it("leaves out the two commands that frame a feedback log", async () => {
    await feedbackRecordStart();
    await feedbackRecordStop();
    expect(logged()).toEqual([]);
  });

  it("still logs them when they fail", async () => {
    rawInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "feedback_record_start") throw "could not open";
      return null;
    });
    await expect(feedbackRecordStart()).rejects.toBe("could not open");
    expect(logged().some((m) => m.startsWith("← feedback_record_start error"))).toBe(true);
  });

  it("logs an ordinary command's request and answer", async () => {
    await feedbackRecording();
    expect(logged()).toEqual([
      expect.stringMatching(/^→ feedback_recording/),
      expect.stringMatching(/^← feedback_recording ok/),
    ]);
  });
});
