import { type RefObject } from "react";
import { XIcon, MagnifyingGlassIcon } from "@phosphor-icons/react";
import { t } from "../../locale/en";

/* The filter field. Typing narrows the list as it goes; Escape or the small
   cross clears it. In a narrow pane it shrinks to its icon until it is used. */
export function SearchBox({
  value,
  onChange,
  inputRef,
  onExit,
  narrow = false,
  open = false,
  onOpenChange,
}: {
  value: string;
  onChange: (v: string) => void;
  /** So the keyboard shortcut can put the cursor here. */
  inputRef?: RefObject<HTMLInputElement>;
  /** Called when the field is left, so the cursor can return to the list. */
  onExit?: () => void;
  /** The pane is too narrow for the full field. */
  narrow?: boolean;
  /** Whether the user opened it anyway. */
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
}) {
  // Collapsed and expanded are the same element, so the width can animate
  // between them.
  const collapsed = narrow && !open;
  const expand = () => {
    onOpenChange?.(true);
    requestAnimationFrame(() => {
      inputRef?.current?.focus();
      inputRef?.current?.select();
    });
  };

  return (
    <div
      onClick={collapsed ? expand : undefined}
      role={collapsed ? "button" : undefined}
      aria-label={collapsed ? t("panel.filter") : undefined}
      className={
        "flex shrink-0 items-center gap-[5px] overflow-hidden rounded-sm border py-1 transition-[width,border-color,padding] duration-200 focus-within:border-accent " +
        (collapsed
          ? "w-[26px] cursor-pointer justify-center px-0 " +
            (value ? "border-accent" : "border-neutral-800")
          : "w-[132px] px-2 border-neutral-800")
      }
    >
      <MagnifyingGlassIcon
        size={12}
        className={
          "shrink-0 " +
          (collapsed && value ? "text-accent-300" : "text-neutral-600")
        }
      />
      <input
        ref={inputRef}
        type="text"
        data-filter=""
        tabIndex={collapsed ? -1 : 0}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onFocus={() => onOpenChange?.(true)}
        onBlur={() => onOpenChange?.(false)}
        onKeyDown={(e) => {
          // Enter leaves the field and keeps the filter; Escape clears it. The
          // event stops here, or the list would read that Enter as "open".
          if (
            e.key === "Enter" ||
            ((e.ctrlKey || e.metaKey) &&
              !e.altKey &&
              !e.shiftKey &&
              e.key.toLowerCase() === "f")
          ) {
            e.preventDefault();
            e.stopPropagation();
            e.currentTarget.blur();
            onExit?.();
          } else if (e.key === "Escape") {
            e.stopPropagation();
            onChange("");
            e.currentTarget.blur();
            onExit?.();
          }
        }}
        placeholder={t("panel.filter")}
        /* The focus outline belongs to the box around the field, magnifier
           included, rather than to the field itself. Collapsed, the field takes
           no room at all, which centres the icon. */
        className={
          "min-w-0 flex-1 bg-transparent text-[11.5px] text-neutral-200 outline-none focus-visible:outline-none placeholder:text-neutral-600 " +
          (collapsed ? "hidden" : "")
        }
      />
      {value && !collapsed && (
        <button
          type="button"
          onMouseDown={(e) => e.preventDefault()} // clearing must not blur the field
          onClick={() => onChange("")}
          aria-label={t("panel.filterClear")}
          className="shrink-0 text-neutral-600 hover:text-neutral-300"
        >
          <XIcon size={11} />
        </button>
      )}
    </div>
  );
}
