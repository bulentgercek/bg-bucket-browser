import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { getVersion } from "@tauri-apps/api/app";
import {
  ArrowLeftIcon,
  PlusIcon,
  HardDrivesIcon,
  PaintBrushIcon,
  InfoIcon,
  ChatCircleTextIcon,
  CheckCircleIcon,
  WarningCircleIcon,
  CircleNotchIcon,
  type Icon,
} from "@phosphor-icons/react";
import {
  testConnection,
  saveConnection,
  renameConnection,
  deleteConnection,
  listOrphanUploads,
  abortOrphanUploads,
  openUrl,
  setRunpodApiKey,
  hasRunpodApiKey,
  isOpenHereRegistered,
  registerOpenHere,
  type ConnectionInfo,
  type TestErr,
  type OrphanUpload,
} from "../../lib/commands";
import { useUiStore } from "../../state/uiStore";
import { usePaneStore } from "../../state/paneStore";
import { useConnectionStore } from "../../state/connectionStore";
import { usePinStore } from "../../state/pinStore";
import { useDialogStore } from "../../state/dialogStore";
import { useToastStore } from "../../state/toastStore";
import { formatSize } from "../../lib/format";
import { IS_WINDOWS, IS_MACOS } from "../../lib/platform";
import { t } from "../../locale/en";
import FeedbackSection from "./FeedbackSection";

/* The Settings screen: connections, appearance, and what this app is.

   Each connection is its own block, and adding one starts a draft that exists
   only here until it is saved. Which connection is live is chosen in the
   sidebar, not here.

   The unfinished-uploads tool lives inside the block of the live connection,
   because that is the one it can reach. */

function NavItem({
  icon: IconCmp,
  label,
  active = false,
  onClick,
}: {
  icon: Icon;
  label: string;
  active?: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={
        "flex items-center gap-2 rounded-sm px-2 py-1.5 text-left text-[12.5px] " +
        (active
          ? "bg-accent-900 text-accent-200"
          : "text-neutral-400 hover:bg-neutral-900")
      }
    >
      <IconCmp size={14} />
      <span>{label}</span>
    </button>
  );
}

function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="field">
      <label>{label}</label>
      {children}
    </div>
  );
}

/* Where a connection test has got to. */
type TestState =
  | { status: "idle" }
  | { status: "loading" }
  | { status: "ok"; ms: number; bucket: string }
  | { status: "error"; err: TestErr };

function TestResult({ state }: { state: TestState }) {
  switch (state.status) {
    case "idle":
      return null;
    case "loading":
      return (
        <span className="flex items-center gap-1.5 text-[11.5px] text-neutral-400">
          <CircleNotchIcon size={14} className="animate-spin" />
          {t("settings.connection.testing")}
        </span>
      );
    case "ok":
      return (
        <span className="flex items-center gap-1.5 text-[11.5px] text-accent-300">
          <CheckCircleIcon size={14} />
          {t("settings.connection.reached", {
            bucket: state.bucket,
            ms: state.ms,
          })}
        </span>
      );
    case "error": {
      const { kind, detail, code, httpStatus } = state.err;
      const tech = [code, httpStatus && `HTTP ${httpStatus}`]
        .filter(Boolean)
        .join(" · ");
      return (
        <div className="flex max-w-[360px] items-start gap-1.5 text-[11.5px]">
          <WarningCircleIcon size={14} className="mt-px shrink-0 text-neutral-300" />
          <div className="space-y-0.5">
            <div className="text-neutral-200">{t(`testError.${kind}`)}</div>
            {tech && <div className="text-neutral-500">{tech}</div>}
            <div className="leading-snug text-neutral-400">{detail}</div>
          </div>
        </div>
      );
    }
  }
}

/* A tooltip that waits before appearing: one that pops up the instant the
   pointer crosses a control is a nuisance. */
function HoverTip({ text, children }: { text: string; children: ReactNode }) {
  const [show, setShow] = useState(false);
  const timer = useRef<number | null>(null);
  const clear = () => {
    if (timer.current !== null) {
      clearTimeout(timer.current);
      timer.current = null;
    }
  };
  return (
    <span
      className="relative inline-flex"
      onMouseEnter={() => {
        clear();
        timer.current = window.setTimeout(() => setShow(true), 2000);
      }}
      onMouseLeave={() => {
        clear();
        setShow(false);
      }}
    >
      {children}
      {show && (
        <span className="absolute left-1/2 top-full z-50 mt-2 w-[280px] -translate-x-1/2 rounded-md border border-neutral-800 bg-surface px-2.5 py-1.5 text-[11px] leading-snug text-neutral-300 shadow-lg">
          {text}
        </span>
      )}
    </span>
  );
}

/* Unfinished multipart uploads on the live connection: find them, show what
   they are holding, and remove them once the user confirms. */
type OrphanState =
  | { status: "idle" }
  | { status: "scanning" }
  | { status: "scanned"; list: OrphanUpload[] }
  | { status: "cleaning" }
  | { status: "done"; count: number }
  | { status: "error" };

function InterruptedUploads() {
  const [st, setSt] = useState<OrphanState>({ status: "idle" });
  const showDialog = useDialogStore((s) => s.show);
  const closeDialog = useDialogStore((s) => s.close);

  async function scan() {
    setSt({ status: "scanning" });
    try {
      setSt({ status: "scanned", list: await listOrphanUploads() });
    } catch {
      setSt({ status: "error" });
    }
  }

  async function cleanUp(list: OrphanUpload[]) {
    setSt({ status: "cleaning" });
    try {
      const count = await abortOrphanUploads(
        list.map((o) => ({ key: o.key, uploadId: o.uploadId })),
      );
      setSt({ status: "done", count });
    } catch {
      setSt({ status: "error" });
    }
  }

  function confirmCleanUp(list: OrphanUpload[]) {
    const size = formatSize(list.reduce((a, o) => a + o.bytes, 0));
    showDialog({
      kind: "confirm",
      title: t("dialog.cleanUpTitle"),
      body: t("dialog.cleanUpBody", { count: list.length, size }),
      confirmLabel: t("settings.maintenance.cleanUp"),
      danger: true,
      onConfirm: () => {
        closeDialog();
        void cleanUp(list);
      },
    });
  }

  const busy = st.status === "scanning" || st.status === "cleaning";

  return (
    <div className="flex items-center gap-3">
      <HoverTip text={t("settings.maintenance.subline")}>
        <button
          type="button"
          className={
            "btn btn-ghost text-[12.5px] " +
            (busy ? "pointer-events-none opacity-40" : "")
          }
          disabled={busy}
          onClick={scan}
        >
          {st.status === "scanning"
            ? t("settings.maintenance.scanning")
            : st.status === "cleaning"
              ? t("settings.maintenance.cleaning")
              : t("settings.maintenance.scan")}
        </button>
      </HoverTip>

      {st.status === "scanned" &&
        (st.list.length === 0 ? (
          <span className="flex items-center gap-1.5 text-[11.5px] text-neutral-500">
            <CheckCircleIcon size={14} className="text-accent-400" />
            {t("settings.maintenance.none")}
          </span>
        ) : (
          <>
            <span className="text-[11.5px] text-neutral-400">
              {t("settings.maintenance.found", {
                count: st.list.length,
                size: formatSize(st.list.reduce((a, o) => a + o.bytes, 0)),
              })}
            </span>
            <button
              type="button"
              className="btn btn-primary text-[12.5px]"
              onClick={() => confirmCleanUp(st.list)}
            >
              {t("settings.maintenance.cleanUp")}
            </button>
          </>
        ))}

      {st.status === "done" && (
        <span className="flex items-center gap-1.5 text-[11.5px] text-accent-300">
          <CheckCircleIcon size={14} />
          {t("settings.maintenance.done", { count: st.count })}
        </span>
      )}
      {st.status === "error" && (
        <span className="flex items-center gap-1.5 text-[11.5px] text-neutral-400">
          <WarningCircleIcon size={14} className="text-neutral-300" />
          {t("settings.maintenance.failed")}
        </span>
      )}
    </div>
  );
}

// What a new connection form starts with.
const DEFAULT_CONN = {
  endpoint: "https://s3api-eu-ro-1.runpod.io",
  region: "EU-RO-1",
  bucket: "",
};

/* One connection's form; without a connection it is an unsaved draft. */
function ConnectionBlock({
  conn,
  draftId,
  isFirst,
  isActive,
  onChanged,
  onDiscardDraft,
}: {
  conn: ConnectionInfo | null;
  /** A draft's own id, which is how the screen drops it from its draft list
      once it has been saved. */
  draftId?: string;
  isFirst: boolean;
  isActive: boolean;
  /** Tells the screen to reload after a save, a rename or a deletion. */
  onChanged: (id: string) => void;
  /** Throws away a draft that was never saved. */
  onDiscardDraft?: () => void;
}) {
  const draft = conn === null;
  const [id] = useState(() => conn?.id ?? draftId ?? crypto.randomUUID());
  const [name, setName] = useState(
    conn?.name ?? t("settings.connections.newName"),
  );
  const [endpoint, setEndpoint] = useState(
    conn?.endpoint || DEFAULT_CONN.endpoint,
  );
  const [region, setRegion] = useState(conn?.region || DEFAULT_CONN.region);
  const [bucket, setBucket] = useState(conn?.bucket ?? DEFAULT_CONN.bucket);
  const [accessKey, setAccessKey] = useState("");
  const [secretKey, setSecretKey] = useState("");
  const [saving, setSaving] = useState(false);
  const [test, setTest] = useState<TestState>({ status: "idle" });

  // A saved block follows its connection when it changes elsewhere.
  useEffect(() => {
    if (!conn) return;
    setName(conn.name);
    setEndpoint(conn.endpoint || DEFAULT_CONN.endpoint);
    setRegion(conn.region || DEFAULT_CONN.region);
    setBucket(conn.bucket);
  }, [conn]);

  // ── inline rename ──
  const [renaming, setRenaming] = useState(false);
  const [renameVal, setRenameVal] = useState("");
  const renameRef = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (renaming) {
      renameRef.current?.focus();
      renameRef.current?.select();
    }
  }, [renaming]);
  const startRename = (withBucket: boolean) => {
    // Renaming with the bucket id replaces the name rather than extending it.
    setRenameVal(withBucket && bucket ? bucket : name);
    setRenaming(true);
  };
  const commitRename = () => {
    const v = renameVal.trim();
    setRenaming(false);
    if (!v || v === name) return;
    setName(v);
    if (!draft) void renameConnection(id, v).then(() => onChanged(id));
  };

  const base = conn ?? {
    name: t("settings.connections.newName"),
    endpoint: DEFAULT_CONN.endpoint,
    region: DEFAULT_CONN.region,
    bucket: DEFAULT_CONN.bucket,
  };
  const dirty =
    draft ||
    name !== base.name ||
    endpoint !== base.endpoint ||
    region !== base.region ||
    bucket !== base.bucket ||
    accessKey.length > 0 ||
    secretKey.length > 0;

  async function save() {
    setSaving(true);
    try {
      await saveConnection({
        id,
        name,
        endpoint,
        region,
        bucket,
        accessKey: accessKey || undefined,
        secretKey: secretKey || undefined,
      });
      setAccessKey("");
      setSecretKey("");
      onChanged(id);
    } finally {
      setSaving(false);
    }
  }

  async function runTest() {
    setTest({ status: "loading" });
    try {
      const { ms, bucket: reached } = await testConnection({
        id: draft ? undefined : id,
        endpoint,
        region,
        bucket,
        accessKey: accessKey || undefined,
        secretKey: secretKey || undefined,
      });
      setTest({ status: "ok", ms, bucket: reached });
    } catch (err) {
      const e = (err ?? {}) as Partial<TestErr>;
      setTest({
        status: "error",
        err: {
          kind: e.kind ?? "unknown",
          detail: e.detail ?? String(err),
          code: e.code ?? null,
          httpStatus: e.httpStatus ?? null,
        },
      });
    }
  }

  function remove() {
    useDialogStore.getState().show({
      kind: "confirm",
      title: t("settings.connections.deleteTitle"),
      body: t("settings.connections.deleteBody", { name }),
      confirmLabel: t("settings.connections.delete"),
      danger: true,
      onConfirm: () => {
        useDialogStore.getState().close();
        void deleteConnection(id).then(() => {
          // A remote pin belongs to its connection and goes with it.
          usePinStore.getState().removePinsForConn(id);
          onChanged(id);
        });
      },
    });
  }

  return (
    <div className="flex flex-col gap-[14px] rounded-md border border-neutral-900 bg-[color-mix(in_srgb,var(--color-surface)_35%,transparent)] px-4 py-3.5">
      {/* ── The name, renamed in place ── */}
      <div className="flex min-h-[24px] items-center gap-2">
        {renaming ? (
          <input
            ref={renameRef}
            defaultValue={renameVal}
            onChange={(e) => setRenameVal(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") commitRename();
              else if (e.key === "Escape") setRenaming(false);
            }}
            onBlur={commitRename}
            className="min-w-0 flex-1 rounded-[3px] border border-accent-600 bg-transparent px-1 py-0.5 text-[13px] font-medium text-text outline-none focus-visible:outline-none"
          />
        ) : (
          <>
            <span className="rounded-[3px] border border-transparent px-1 py-0.5 text-[13px] font-medium text-neutral-200">
              {name}
            </span>
            <button
              type="button"
              onClick={() => startRename(false)}
              className="text-[11px] text-neutral-500 hover:text-neutral-300"
            >
              {t("settings.connections.rename")}
            </button>
            <span className="text-neutral-800">·</span>
            <button
              type="button"
              onClick={() => startRename(true)}
              className="text-[11px] text-neutral-500 hover:text-neutral-300"
            >
              {t("settings.connections.renameWithBucket")}
            </button>
            {isActive && (
              <span className="ml-auto rounded-full border border-accent-700 px-1.5 py-px text-[9.5px] uppercase tracking-wide text-accent-300">
                {t("settings.connections.active")}
              </span>
            )}
          </>
        )}
      </div>

      {/* ── alanlar ── */}
      <div className="flex flex-col gap-[14px]">
        <Field label={t("settings.field.endpointUrl")}>
          <input
            className="input"
            value={endpoint}
            onChange={(e) => setEndpoint(e.target.value)}
          />
        </Field>
        <div className="grid grid-cols-2 gap-[14px]">
          <Field label={t("settings.field.accessKeyId")}>
            <input
              className="input"
              value={accessKey}
              onChange={(e) => setAccessKey(e.target.value)}
              placeholder={
                conn?.hasAccessKey ? t("settings.field.keyStored") : "user_…"
              }
            />
          </Field>
          <Field label={t("settings.field.secretAccessKey")}>
            <input
              className="input"
              type="password"
              value={secretKey}
              onChange={(e) => setSecretKey(e.target.value)}
              placeholder={
                conn?.hasSecretKey ? t("settings.field.keyStored") : "rps_…"
              }
            />
          </Field>
        </div>
        <div className="grid grid-cols-2 gap-[14px]">
          <Field label={t("settings.field.bucketId")}>
            <input
              className="input"
              value={bucket}
              onChange={(e) => setBucket(e.target.value)}
            />
          </Field>
          <Field label={t("settings.field.region")}>
            <input
              className="input"
              value={region}
              onChange={(e) => setRegion(e.target.value)}
            />
          </Field>
        </div>
      </div>

      {/* ── Test, unfinished uploads, and what came back ── */}
      <div className="flex flex-wrap items-center gap-3">
        <button
          type="button"
          className="btn btn-primary shrink-0 text-[12.5px]"
          onClick={runTest}
          disabled={test.status === "loading"}
        >
          {t("settings.connection.test")}
        </button>
        {isActive && <InterruptedUploads />}
        <div className="basis-full" />
        <TestResult state={test} />
      </div>

      {/* ── alt: Discard/Delete + Save ── */}
      <div className="flex items-center justify-between">
        {draft ? (
          <button
            type="button"
            onClick={onDiscardDraft}
            className="text-[11.5px] text-neutral-500 hover:text-neutral-300"
          >
            {t("settings.connections.discard")}
          </button>
        ) : isFirst ? (
          <span />
        ) : (
          <button
            type="button"
            onClick={remove}
            className="text-[11.5px] text-neutral-500 hover:text-[var(--color-danger)]"
          >
            {t("settings.connections.delete")}
          </button>
        )}
        <button
          type="button"
          className={
            "btn btn-primary " +
            (!dirty || saving ? "pointer-events-none opacity-40" : "")
          }
          disabled={!dirty || saving}
          onClick={save}
        >
          {t("settings.action.save")}
        </button>
      </div>
    </div>
  );
}

/* The RunPod account key: not an S3 key and not part of any connection, since
   every volume in one account is read with the same one. */
function RunpodKeyBlock() {
  const [key, setKey] = useState("");
  const [stored, setStored] = useState(false);
  const [saving, setSaving] = useState(false);

  const refresh = () => {
    void hasRunpodApiKey()
      .then(setStored)
      .catch(() => setStored(false));
  };
  useEffect(refresh, []);

  const save = () => {
    setSaving(true);
    setRunpodApiKey(key)
      .then(() => {
        useToastStore
          .getState()
          .push(t(key.trim() ? "settings.runpod.saved" : "settings.runpod.cleared"));
        setKey("");
        refresh();
      })
      .catch((e: unknown) => useToastStore.getState().push(String(e), "error"))
      .finally(() => setSaving(false));
  };

  return (
    <div className="flex flex-col gap-2 rounded-md border border-neutral-800 p-4">
      <div>
        <div className="text-[13px]">{t("settings.runpod.label")}</div>
        <p className="mt-0.5 max-w-[520px] text-[11.5px] text-neutral-500">
          {t("settings.runpod.subline")}
        </p>
      </div>
      <div className="flex items-end gap-[10px]">
        <Field label={t("settings.field.accessKeyId")}>
          <input
            className="input"
            type="password"
            value={key}
            onChange={(e) => setKey(e.target.value)}
            placeholder={stored ? t("settings.field.keyStored") : "rpa_…"}
          />
        </Field>
        <button
          type="button"
          className={
            "btn btn-primary " +
            (saving || key.trim() === "" ? "pointer-events-none opacity-40" : "")
          }
          disabled={saving || key.trim() === ""}
          onClick={save}
        >
          {t("settings.runpod.save")}
        </button>
      </div>
    </div>
  );
}

export default function SettingsScreen() {
  const setScreen = useUiStore((s) => s.setScreen);
  const nav = useUiStore((s) => s.settingsTab);
  const setNav = useUiStore((s) => s.setSettingsTab);
  const theme = useUiStore((s) => s.theme);
  const setTheme = useUiStore((s) => s.setTheme);
  const osDropMode = useUiStore((s) => s.osDropMode);
  const setOsDropMode = useUiStore((s) => s.setOsDropMode);

  const connections = useConnectionStore((s) => s.connections);
  const activeId = useConnectionStore((s) => s.activeId);
  const reloadConns = useConnectionStore((s) => s.load);

  const [drafts, setDrafts] = useState<string[]>([]);
  const [version, setVersion] = useState("");
  // The real state is read when the screen opens: the registration can be
  // added or removed outside the app.
  const [openHere, setOpenHere] = useState(false);
  useEffect(() => {
    void isOpenHereRegistered().then(setOpenHere);
  }, []);
  const toggleOpenHere = (next: boolean) => {
    setOpenHere(next); // switched straight away and put back if it fails
    registerOpenHere(next)
      .then(() =>
        useToastStore
          .getState()
          .push(
            t(
              IS_MACOS
                ? next
                  ? "settings.openHere.on.mac"
                  : "settings.openHere.off.mac"
                : next
                  ? "settings.openHere.on"
                  : "settings.openHere.off",
            ),
          ),
      )
      .catch((e: unknown) => {
        setOpenHere(!next); // put the switch back
        useToastStore.getState().push(String(e), "error");
      });
  };
  useEffect(() => {
    getVersion()
      .then(setVersion)
      .catch(() => setVersion(""));
  }, []);
  // A fresh list whenever the screen opens.
  useEffect(() => {
    void reloadConns();
  }, [reloadConns]);

  const onBlockChanged = (changedId: string) => {
    setDrafts((d) => d.filter((x) => x !== changedId));
    void reloadConns().then(() => {
      // The live connection may now point somewhere else.
      void usePaneStore.getState().refresh(0);
    });
  };

  return (
    <div className="flex h-full flex-col bg-bg text-text">
      {/* ── Title bar ── */}
      <div className="flex items-center justify-between border-b border-neutral-900 bg-titlebar px-[14px] py-[9px]">
        <div className="flex items-baseline gap-2">
          <button
            type="button"
            onClick={() => setScreen("main")}
            className="text-[13px] font-medium tracking-[-0.01em] hover:text-accent-200"
          >
            {t("app.name")}
          </button>
          <span className="text-[11.5px] text-neutral-500">
            {t("settings.title")}
          </span>
        </div>
        <button
          type="button"
          className="flex items-center gap-1.5 rounded-sm border border-neutral-800 px-2.5 py-1 text-[11.5px] text-neutral-400 hover:border-accent-700 hover:text-accent-300"
          onClick={() => setScreen("main")}
        >
          <ArrowLeftIcon size={13} />
          {t("settings.back")}
        </button>
      </div>

      {/* ── Body: 196px nav + content ── */}
      <div className="grid min-h-[420px] flex-1 grid-cols-[196px_1fr] overflow-hidden">
        <nav className="flex flex-col gap-0.5 bg-chrome px-2.5 py-4">
          <NavItem
            icon={HardDrivesIcon}
            label={t("settings.nav.connections")}
            active={nav === "connections"}
            onClick={() => setNav("connections")}
          />
          <NavItem
            icon={PaintBrushIcon}
            label={t("settings.nav.appearance")}
            active={nav === "appearance"}
            onClick={() => setNav("appearance")}
          />
          <NavItem
            icon={ChatCircleTextIcon}
            label={t("settings.nav.feedback")}
            active={nav === "feedback"}
            onClick={() => setNav("feedback")}
          />
          <NavItem
            icon={InfoIcon}
            label={t("settings.nav.about")}
            active={nav === "about"}
            onClick={() => setNav("about")}
          />
        </nav>

        <div className="overflow-y-auto">
          {nav === "appearance" && (
            <div className="flex max-w-[640px] flex-col gap-[22px] px-[34px] py-[26px]">
              <div>
                <h4 className="text-[20px]">
                  {t("settings.appearance.heading")}
                </h4>
                <p className="mt-1 text-[12px] text-neutral-500">
                  {t("settings.appearance.subline")}
                </p>
              </div>

              <div className="flex items-center justify-between">
                <div>
                  <div className="text-[13px]">{t("settings.theme.label")}</div>
                  <div className="text-[11.5px] text-neutral-500">
                    {t("settings.theme.subline")}
                  </div>
                </div>
                <div className="seg">
                  {(["dark", "light", "system"] as const).map((val) => (
                    <label key={val} className="seg-opt">
                      <input
                        type="radio"
                        name="theme"
                        checked={theme === val}
                        onChange={() => setTheme(val)}
                      />
                      {t(`settings.theme.${val}`)}
                    </label>
                  ))}
                </div>
              </div>

              {/* Hidden where it would do nothing: dropping files onto the
                  window only works on Linux, since the setting that enables it
                  disables dragging between the panes on the other two
                  platforms. */}
              {!IS_WINDOWS && !IS_MACOS && (
                <>
                  <div className="h-px bg-neutral-900" />

                  <div className="flex items-center justify-between">
                    <div>
                      <div className="text-[13px]">
                        {t("settings.osDrop.label")}
                      </div>
                      <div className="max-w-[360px] text-[11.5px] text-neutral-500">
                        {t("settings.osDrop.subline")}
                      </div>
                    </div>
                    <div className="seg">
                      {(["copy", "menu"] as const).map((val) => (
                        <label key={val} className="seg-opt">
                          <input
                            type="radio"
                            name="osDropMode"
                            checked={osDropMode === val}
                            onChange={() => setOsDropMode(val)}
                          />
                          {t(`settings.osDrop.${val}`)}
                        </label>
                      ))}
                    </div>
                  </div>
                </>
              )}

              <div className="h-px bg-neutral-900" />

              <div className="flex items-center justify-between">
                <div>
                  <div className="text-[13px]">
                    {t("settings.openHere.label")}
                  </div>
                  <div className="max-w-[360px] text-[11.5px] text-neutral-500">
                    {t(
                      IS_MACOS
                        ? "settings.openHere.subline.mac"
                        : "settings.openHere.subline",
                    )}
                  </div>
                </div>
                <label className="flex shrink-0 items-center gap-2 text-[12.5px] text-neutral-300">
                  <input
                    type="checkbox"
                    checked={openHere}
                    onChange={(e) => toggleOpenHere(e.target.checked)}
                  />
                  {t("settings.openHere.toggle")}
                </label>
              </div>
            </div>
          )}

          {nav === "feedback" && <FeedbackSection />}

          {nav === "about" && (
            <div className="flex max-w-[640px] flex-col gap-[14px] px-[34px] py-[26px]">
              <h4 className="text-[20px]">{t("settings.about.heading")}</h4>
              <div className="text-[13px] text-neutral-200">
                {t("app.name")}
              </div>
              <div className="text-[12px] text-neutral-500">
                {t("settings.about.version", { version: version || "—" })}
              </div>
              <p className="max-w-[440px] text-[12px] leading-relaxed text-neutral-400">
                {t("settings.about.tagline")}
              </p>
              <div className="text-[11.5px] text-neutral-600">
                {t(
                  IS_WINDOWS
                    ? "settings.about.platform.windows"
                    : IS_MACOS
                      ? "settings.about.platform.mac"
                      : "settings.about.platform",
                )}
              </div>

              <div className="mt-1 h-px w-[440px] bg-neutral-900" />

              <div className="text-[12px] text-neutral-300">
                {t("settings.about.createdBy")}
              </div>
              <div className="max-w-[440px] space-y-1 text-[12px] leading-relaxed text-neutral-400">
                <p>
                  {(() => {
                    const EMAIL = "bulentgercek@gmail.com";
                    const [before, after] = t("settings.about.contact", {
                      email: EMAIL,
                    }).split(EMAIL);
                    return (
                      <>
                        {before}
                        <button
                          type="button"
                          onClick={() => {
                            void openUrl(`mailto:${EMAIL}`);
                          }}
                          className="text-accent-300 underline decoration-accent-700 underline-offset-2 hover:decoration-accent-400"
                        >
                          {EMAIL}
                        </button>
                        {after}
                      </>
                    );
                  })()}
                </p>
                <p>{t("settings.about.hope")}</p>
                <p>{t("settings.about.day")}</p>
              </div>
            </div>
          )}

          {nav === "connections" && (
            <div className="flex max-w-[660px] flex-col gap-[18px] px-[34px] py-[26px]">
              <div>
                <h4 className="text-[20px]">
                  {t("settings.connections.heading")}
                </h4>
                <p className="mt-1 text-[12px] text-neutral-500">
                  {t("settings.connection.subline")}
                </p>
              </div>

              {connections.map((c) => (
                <ConnectionBlock
                  key={c.id}
                  conn={c}
                  isFirst={connections[0]?.id === c.id}
                  isActive={c.id === activeId}
                  onChanged={onBlockChanged}
                />
              ))}

              {drafts.map((dId) => (
                <ConnectionBlock
                  key={dId}
                  conn={null}
                  draftId={dId}
                  isFirst={false}
                  isActive={false}
                  onChanged={onBlockChanged}
                  onDiscardDraft={() =>
                    setDrafts((d) => d.filter((x) => x !== dId))
                  }
                />
              ))}

              <button
                type="button"
                onClick={() =>
                  setDrafts((d) => [...d, crypto.randomUUID()])
                }
                className="flex items-center gap-2 self-start rounded-md border border-dashed border-neutral-800 px-3 py-2 text-[12.5px] text-neutral-400 hover:border-accent-700 hover:text-accent-300"
              >
                <PlusIcon size={14} />
                {t("settings.connections.add")}
              </button>

              <div className="h-px bg-neutral-900" />
              <RunpodKeyBlock />
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
