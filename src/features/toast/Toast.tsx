import { createPortal } from "react-dom";
import { CheckCircleIcon, WarningCircleIcon } from "@phosphor-icons/react";
import { useToastStore } from "../../state/toastStore";

/* The notices stacked in the corner; clicking one dismisses it. */
export default function Toast() {
  const toasts = useToastStore((s) => s.toasts);
  const dismiss = useToastStore((s) => s.dismiss);
  if (toasts.length === 0) return null;

  return createPortal(
    <div className="fixed bottom-4 right-4 z-[200] flex flex-col gap-2">
      {toasts.map((t) => (
        <button
          key={t.id}
          type="button"
          onClick={() => dismiss(t.id)}
          className={
            "flex max-w-[340px] items-start gap-2 rounded-md border bg-surface px-3 py-2 text-left text-[12px] shadow-lg " +
            (t.tone === "error"
              ? "border-neutral-700 border-l-2 border-l-[var(--color-danger)] text-neutral-200"
              : "border-neutral-800 text-neutral-300")
          }
        >
          {t.tone === "error" ? (
            <WarningCircleIcon size={14} className="mt-px shrink-0 text-neutral-300" />
          ) : (
            <CheckCircleIcon size={14} className="mt-px shrink-0 text-accent-400" />
          )}
          <span className="leading-snug">{t.text}</span>
        </button>
      ))}
    </div>,
    document.body,
  );
}
