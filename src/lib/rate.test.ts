import { describe, expect, it } from "vitest";
import { addSample, rateOf, type RateSample } from "./rate";

const MB = 1_000_000;

function reports(list: [number, number][]): RateSample[] {
  return list.reduce<RateSample[]>((acc, [at, bytes]) => addSample(acc, { at, bytes }), []);
}

describe("rateOf", () => {
  it("is zero before anything moved", () => {
    expect(rateOf([], 5000)).toBe(0);
    expect(rateOf(reports([[0, 0]]), 0)).toBe(0);
    expect(rateOf(reports([[0, 0]]), 3000)).toBe(0);
  });

  it("measures a young transfer from its first report", () => {
    expect(rateOf(reports([[0, 0], [2000, 10 * MB]]), 2000)).toBe(5 * MB);
  });

  it("follows the recent pace, not the average since the start", () => {
    // Twenty seconds at 1 MB/s, then ten at 5 MB/s.
    const s = reports([
      [0, 0],
      [10_000, 10 * MB],
      [20_000, 20 * MB],
      [25_000, 45 * MB],
      [30_000, 70 * MB],
    ]);
    expect(rateOf(s, 30_000)).toBe(5 * MB);
  });

  it("stays steady between the reports of a chunked upload", () => {
    // One 8 MB part every two seconds.
    const s = reports([0, 1, 2, 3, 4, 5, 6].map((i) => [i * 2000, i * 8 * MB] as [number, number]));
    expect(rateOf(s, 12_000)).toBe(4 * MB);
    expect(rateOf(s, 13_000)).toBe(4 * MB);
  });

  it("falls to zero when the reports stop", () => {
    const s = reports([[0, 0], [2000, 10 * MB], [4000, 20 * MB]]);
    expect(rateOf(s, 4000)).toBe(5 * MB);
    expect(rateOf(s, 9000)).toBeGreaterThan(0);
    expect(rateOf(s, 14_000)).toBe(0);
  });
});

describe("addSample", () => {
  it("keeps one report from before the window as its start", () => {
    const s = reports([[0, 0], [5000, 1], [11_000, 2], [12_000, 3]]);
    expect(s.map((x) => x.at)).toEqual([0, 5000, 11_000, 12_000]);
    const t = addSample(s, { at: 16_000, bytes: 4 });
    expect(t.map((x) => x.at)).toEqual([5000, 11_000, 12_000, 16_000]);
  });
});
