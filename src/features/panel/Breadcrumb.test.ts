import { describe, expect, it, vi } from "vitest";

// These tests call one pure function and render nothing, but importing the
// component pulls in its icon package, which is by far the slowest import in
// the chain. Stubbing it keeps the first import inside the default timeout.
vi.mock("@phosphor-icons/react", () => ({
  CloudIcon: () => null,
  DesktopTowerIcon: () => null,
  FolderIcon: () => null,
}));

// The platform is read once, when the module loads, so each test loads the
// module again with the platform it needs.
async function crumbsOn(windows: boolean) {
  vi.resetModules();
  vi.doMock("../../lib/platform", () => ({ IS_WINDOWS: windows, IS_MACOS: false }));
  return (await import("./Breadcrumb")).buildCrumbs;
}

describe("buildCrumbs on Linux and macOS", () => {
  it("starts a remote path at the bucket", async () => {
    const build = await crumbsOn(false);
    expect(build("", true, "bkt", "/home/me")).toEqual([{ label: "bkt", path: "" }]);
    expect(build("a/b", true, "bkt", "/home/me")).toEqual([
      { label: "bkt", path: "" },
      { label: "a", path: "a" },
      { label: "b", path: "a/b" },
    ]);
  });

  it("starts a home path at ~", async () => {
    const build = await crumbsOn(false);
    expect(build("~", false, "bkt", "/home/me")).toEqual([{ label: "~", path: "~" }]);
    expect(build("~/Downloads/x", false, "bkt", "/home/me")).toEqual([
      { label: "~", path: "~" },
      { label: "Downloads", path: "~/Downloads" },
      { label: "x", path: "~/Downloads/x" },
    ]);
  });

  it("starts an absolute path at the filesystem root", async () => {
    const build = await crumbsOn(false);
    expect(build("/", false, "bkt", "/home/me")).toEqual([{ label: "/", path: "/" }]);
    expect(build("/mnt/data", false, "bkt", "/home/me")).toEqual([
      { label: "/", path: "/" },
      { label: "mnt", path: "/mnt" },
      { label: "data", path: "/mnt/data" },
    ]);
  });
});

describe("buildCrumbs on Windows", () => {
  it("shows a ~ path as the real home path", async () => {
    const build = await crumbsOn(true);
    expect(build("~/Docs", false, "bkt", "C:\\Users\\me")).toEqual([
      { label: "C:\\", path: "C:\\" },
      { label: "Users", path: "C:\\Users" },
      { label: "me", path: "C:\\Users/me" },
      { label: "Docs", path: "C:\\Users/me/Docs" },
    ]);
  });

  it("keeps ~ while the home path is not known yet", async () => {
    const build = await crumbsOn(true);
    expect(build("~", false, "bkt", "")).toEqual([{ label: "~", path: "~" }]);
  });

  // A drive root once fell through to the home branch and grew a stray "~" crumb.
  it("makes a drive its own root, with no ~ crumb", async () => {
    const build = await crumbsOn(true);
    for (const path of ["C:", "C:\\", "C:/"]) {
      expect(build(path, false, "bkt", "C:\\Users\\me")).toEqual([{ label: "C:\\", path: "C:\\" }]);
    }
    expect(build("D:\\games/saves", false, "bkt", "C:\\Users\\me")).toEqual([
      { label: "D:\\", path: "D:\\" },
      { label: "games", path: "D:\\games" },
      { label: "saves", path: "D:\\games/saves" },
    ]);
  });

  it("leaves remote paths alone", async () => {
    const build = await crumbsOn(true);
    expect(build("C:/x", true, "bkt", "C:\\Users\\me")).toEqual([
      { label: "bkt", path: "" },
      { label: "C:", path: "C:" },
      { label: "x", path: "C:/x" },
    ]);
  });
});
