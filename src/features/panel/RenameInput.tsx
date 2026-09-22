import { useEffect, useRef } from "react";

/* Renaming in place. A real text field, so selecting a word, selecting all and
   pasting all behave as they do anywhere else; its events are kept from
   reaching the row underneath, which would select or open it.

   Enter and clicking away both accept the name; Escape abandons it. */
export function RenameInput({
  initial,
  center,
  onCommit,
  onCancel,
}: {
  initial: string;
  center?: boolean;
  onCommit: (value: string) => void;
  onCancel: () => void;
}) {
  const ref = useRef<HTMLInputElement>(null);
  const done = useRef(false);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.focus();
    el.select();
  }, []);
  return (
    <input
      ref={ref}
      defaultValue={initial}
      spellCheck={false}
      style={{ fontSize: "inherit" }}
      onClick={(e) => e.stopPropagation()}
      onDoubleClick={(e) => e.stopPropagation()}
      onMouseDown={(e) => e.stopPropagation()}
      onContextMenu={(e) => e.stopPropagation()}
      onKeyDown={(e) => {
        e.stopPropagation();
        if (e.key === "Enter") {
          done.current = true;
          onCommit(e.currentTarget.value);
        } else if (e.key === "Escape") {
          done.current = true;
          onCancel();
        }
      }}
      onBlur={(e) => {
        if (done.current) return;
        done.current = true;
        onCommit(e.currentTarget.value);
      }}
      className={
        "min-w-0 flex-1 rounded-[3px] border border-accent-600 bg-transparent px-1 leading-tight text-text outline-none " +
        (center ? "text-center" : "")
      }
    />
  );
}
