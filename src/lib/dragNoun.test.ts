import { describe, expect, it } from "vitest";
import { dragNounKey } from "./dragNoun";

const file = { kind: "file" } as const;
const dir = { kind: "dir" } as const;

describe("the drag badge's noun", () => {
  it("names files as files", () => {
    expect(dragNounKey([file])).toBe("dnd.file");
    expect(dragNounKey([file, file])).toBe("dnd.files");
  });

  it("names folders as folders", () => {
    expect(dragNounKey([dir])).toBe("dnd.folder");
    expect(dragNounKey([dir, dir, dir])).toBe("dnd.folders");
  });

  it("says items when files and folders are dragged together", () => {
    expect(dragNounKey([dir, file])).toBe("dnd.items");
  });
});
