import { join } from "node:path";
import { pathToFileURL } from "node:url";

export type ShellPage = "server" | "picker" | "offline" | "keybind" | "settings";

// electron-vite serves the shell pages from a dev server during `npm run dev`
// and from out/renderer once built.
const devBase = process.env["ELECTRON_RENDERER_URL"];
const builtDir = join(__dirname, "../renderer");

export function shellPageUrl(page: ShellPage, query?: Record<string, string>): string {
  const url = devBase
    ? new URL(`${page}.html`, devBase.endsWith("/") ? devBase : `${devBase}/`)
    : pathToFileURL(join(builtDir, `${page}.html`));
  for (const [k, v] of Object.entries(query ?? {})) url.searchParams.set(k, v);
  return url.toString();
}

/** Whether a frame URL is one of the shell's own pages. */
export function isShellUrl(raw: string | undefined): boolean {
  if (!raw) return false;
  try {
    const url = new URL(raw);
    if (devBase) return url.origin === new URL(devBase).origin;
    return url.protocol === "file:" && url.href.startsWith(pathToFileURL(builtDir).href);
  } catch {
    return false;
  }
}
