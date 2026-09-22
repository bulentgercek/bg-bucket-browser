import { beforeEach, describe, expect, it, vi } from "vitest";
import type { PathStat } from "./commands";

// The backend is replaced by a table of what `stat_paths` would answer.
vi.mock("./commands", () => ({ statPaths: vi.fn() }));
const { statPaths } = await import("./commands");
const { groupPathsByDir } = await import("./osFiles");

// The name as the backend reports it: the last component, whichever separator
// the platform uses.
function stat(path: string, isDir = false, name = path.split(/[\\/]/).pop() ?? path): PathStat {
  return {
    path,
    name,
    isDir,
    size: isDir ? null : 3,
    modified: 1000,
  };
}

describe("groupPathsByDir", () => {
  beforeEach(() => vi.mocked(statPaths).mockReset());

  it("puts files from one folder into one group, in order", async () => {
    vi.mocked(statPaths).mockResolvedValue([stat("/home/me/a.txt"), stat("/home/me/sub", true)]);
    const groups = await groupPathsByDir(["/home/me/a.txt", "/home/me/sub"]);

    expect([...groups.keys()]).toEqual(["/home/me"]);
    expect(groups.get("/home/me")).toEqual([
      { name: "a.txt", kind: "file", size: 3, modified: 1000, glyph: null },
      { name: "sub", kind: "dir", size: null, modified: 1000, glyph: null },
    ]);
  });

  it("gives each source folder its own group", async () => {
    vi.mocked(statPaths).mockResolvedValue([stat("/a/x"), stat("/b/y"), stat("/a/z")]);
    const groups = await groupPathsByDir(["/a/x", "/b/y", "/a/z"]);

    expect([...groups.keys()]).toEqual(["/a", "/b"]);
    expect(groups.get("/a")?.map((e) => e.name)).toEqual(["x", "z"]);
  });

  it("groups a file at the filesystem root under /", async () => {
    vi.mocked(statPaths).mockResolvedValue([stat("/x")]);
    const groups = await groupPathsByDir(["/x"]);

    expect([...groups.keys()]).toEqual(["/"]);
  });

  // Windows hands out paths with backslashes; these once all landed in "/".
  it("finds the folder of a Windows path", async () => {
    const paths = ["C:\\Users\\me\\docs\\a.txt", "C:\\Users\\me\\docs\\b note.txt"];
    vi.mocked(statPaths).mockResolvedValue(paths.map((p) => stat(p)));
    const groups = await groupPathsByDir(paths);

    expect([...groups.keys()]).toEqual(["C:\\Users\\me\\docs"]);
    expect(groups.get("C:\\Users\\me\\docs")?.map((e) => e.name)).toEqual(["a.txt", "b note.txt"]);
  });

  it("makes a drive root a root", async () => {
    vi.mocked(statPaths).mockResolvedValue([stat("C:\\a.txt"), stat("D:\\data", true)]);
    const groups = await groupPathsByDir(["C:\\a.txt", "D:\\data"]);

    expect([...groups.keys()]).toEqual(["C:/", "D:/"]);
  });

  it("keeps a backslash that is part of a Linux name", async () => {
    vi.mocked(statPaths).mockResolvedValue([stat("/home/me/a\\b.txt", false, "a\\b.txt")]);
    const groups = await groupPathsByDir(["/home/me/a\\b.txt"]);

    expect([...groups.keys()]).toEqual(["/home/me"]);
    expect(groups.get("/home/me")?.[0].name).toBe("a\\b.txt");
  });

  it("returns no groups when nothing could be read", async () => {
    vi.mocked(statPaths).mockResolvedValue([]);
    const groups = await groupPathsByDir(["/gone"]);

    expect(groups.size).toBe(0);
  });
});
