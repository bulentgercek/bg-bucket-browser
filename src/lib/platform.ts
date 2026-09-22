/** Whether this is Windows. Only presentation depends on it — how a path is
    written, which drag hint is shown — never storage or navigation. */
export const IS_WINDOWS = navigator.userAgent.includes("Windows");

/** Whether this is macOS; same reasoning and same limits as `IS_WINDOWS`. */
export const IS_MACOS = navigator.userAgent.includes("Macintosh");
