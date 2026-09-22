import { statPaths } from "./commands";
import type { Entry } from "../state/paneStore";

/* Turns raw paths coming from outside the app — a drop on the window, a paste
   from the OS clipboard — into entries grouped by their parent directory.

   A transfer takes one source directory, so paths from several directories
   become several transfers. That is rare: file managers usually hand over one
   directory's worth of files. */
export async function groupPathsByDir(paths: string[]): Promise<Map<string, Entry[]>> {
  const stats = await statPaths(paths);
  const groups = new Map<string, Entry[]>();
  for (const s of stats) {
    const dir = folderOf(s.path, s.name);
    const entry: Entry = {
      name: s.name,
      kind: s.isDir ? "dir" : "file",
      size: s.size,
      modified: s.modified,
      glyph: null,
    };
    const arr = groups.get(dir);
    if (arr) arr.push(entry);
    else groups.set(dir, [entry]);
  }
  return groups;
}

/** The folder a path sits in. The entry's own name is taken off the end rather
    than a separator looked for: Windows writes `\`, while on Linux and macOS a
    `\` is an ordinary character inside a name. A drive root comes back as
    `C:/`, the way the app writes paths. */
function folderOf(path: string, name: string): string {
  const cut = path.length - name.length - 1;
  let dir: string;
  if (name !== "" && cut >= 0 && path.endsWith(name) && (path[cut] === "/" || path[cut] === "\\")) {
    dir = path.slice(0, cut);
  } else {
    const slash = path.lastIndexOf("/");
    dir = slash > 0 ? path.slice(0, slash) : "";
  }
  if (dir === "") return "/";
  if (/^[A-Za-z]:$/.test(dir)) return `${dir}/`;
  return dir;
}
