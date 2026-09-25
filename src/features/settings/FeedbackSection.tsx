import { RecordIcon, XIcon } from "@phosphor-icons/react";
import { useFeedbackStore } from "../../state/feedbackStore";
import { t } from "../../locale/en";

/* Settings → Feedback: the user's message, an optional address for a reply,
   and the recording.

   A recording is attached in a box of its own under the message, headed and
   read-only, so what the user wrote never mixes with the log. Sending is one
   press; nothing has to be pasted or attached by hand. */

/** "42 s", "4 min 12 s". */
export function formatDuration(sec: number): string {
  const m = Math.floor(sec / 60);
  const s = sec % 60;
  return m > 0 ? `${m} min ${s} s` : `${s} s`;
}

export default function FeedbackSection() {
  const message = useFeedbackStore((s) => s.message);
  const contact = useFeedbackStore((s) => s.contact);
  const attached = useFeedbackStore((s) => s.attached);
  const recording = useFeedbackStore((s) => s.recordingSince !== null);
  const sending = useFeedbackStore((s) => s.sending);
  const isTest = useFeedbackStore((s) => s.isTest);
  const { setMessage, setContact, startRecording, removeRecording, send } =
    useFeedbackStore.getState();

  const canSend = !sending && !recording && (message.trim() !== "" || attached !== null);

  return (
    <div className="flex max-w-[640px] flex-col gap-[18px] px-[34px] py-[26px]">
      <div>
        <h4 className="text-[20px]">{t("feedback.heading")}</h4>
        <p className="mt-1 text-[12px] text-neutral-500">{t("feedback.subline")}</p>
        {isTest && (
          <p className="mt-1 text-[11.5px] text-neutral-600">{t("feedback.testChannel")}</p>
        )}
      </div>

      <div className="field">
        <label htmlFor="feedback-message">{t("feedback.message.label")}</label>
        <textarea
          id="feedback-message"
          className="input min-h-[120px] resize-y leading-relaxed"
          value={message}
          maxLength={10000}
          placeholder={t("feedback.message.placeholder")}
          onChange={(e) => setMessage(e.target.value)}
        />
      </div>

      <div className="field max-w-[360px]">
        <label htmlFor="feedback-contact">{t("feedback.contact.label")}</label>
        <input
          id="feedback-contact"
          className="input"
          type="email"
          value={contact}
          maxLength={254}
          onChange={(e) => setContact(e.target.value)}
        />
        <div className="mt-1 text-[11.5px] text-neutral-500">{t("feedback.contact.hint")}</div>
      </div>

      {attached ? (
        <div className="flex flex-col rounded-md border border-neutral-800">
          <div className="flex items-center justify-between gap-3 border-b border-neutral-800 px-3 py-2">
            <div className="min-w-0">
              <div className="text-[12.5px] text-neutral-200">
                {t("feedback.recording.title")}
              </div>
              <div className="text-[11.5px] text-neutral-500">
                {t("feedback.recording.meta", {
                  duration: formatDuration(attached.durationSec),
                  lines: attached.lines.toLocaleString("en-US"),
                })}
                {attached.cutShort && ` · ${t("feedback.recording.cutShort")}`}
              </div>
            </div>
            <button
              type="button"
              onClick={() => void removeRecording()}
              className="flex shrink-0 items-center gap-1 text-[11.5px] text-neutral-500 hover:text-neutral-200"
            >
              <XIcon size={12} />
              {t("feedback.recording.remove")}
            </button>
          </div>
          <pre className="max-h-[220px] overflow-auto whitespace-pre px-3 py-2 font-mono text-[11px] leading-[1.5] text-neutral-400">
            {attached.text}
          </pre>
        </div>
      ) : (
        <div className="flex flex-col items-start gap-1.5">
          <button
            type="button"
            disabled={recording}
            onClick={() => void startRecording()}
            className="btn border-[var(--color-danger)] text-[13px] text-[var(--color-danger)] hover:bg-[color-mix(in_srgb,var(--color-danger)_12%,transparent)]"
          >
            <RecordIcon size={14} weight="fill" />
            {t("feedback.record")}
          </button>
          <p className="max-w-[520px] text-[11.5px] text-neutral-500">
            {t("feedback.record.howto")}
          </p>
          <p className="max-w-[520px] text-[11.5px] text-[var(--color-danger)]">
            {t("feedback.record.warning")}
          </p>
        </div>
      )}

      <div className="flex justify-end">
        <button
          type="button"
          className="btn btn-primary"
          disabled={!canSend}
          onClick={() => void send()}
        >
          {sending ? t("feedback.sending") : t("feedback.send")}
        </button>
      </div>
    </div>
  );
}
