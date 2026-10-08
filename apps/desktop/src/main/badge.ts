import { app, nativeImage, type BrowserWindow, type NativeImage } from "electron";

/**
 * The client keeps its unread count in the title as "(N) Chatter"
 * (client/src/lib/store/provider.tsx). Reading it from there means the badge
 * works against any Chatter server without a client change.
 */
export function unreadFromTitle(title: string): number {
  const match = /^\((\d+)\)/.exec(title);
  return match ? Number(match[1]) : 0;
}

// 3x5 pixel glyphs, one row per string, for drawing the count without a font
// renderer in the main process.
const GLYPHS: Record<string, string[]> = {
  "0": ["111", "101", "101", "101", "111"],
  "1": ["010", "110", "010", "010", "111"],
  "2": ["111", "001", "111", "100", "111"],
  "3": ["111", "001", "111", "001", "111"],
  "4": ["101", "101", "111", "001", "001"],
  "5": ["111", "100", "111", "001", "111"],
  "6": ["111", "100", "111", "101", "111"],
  "7": ["111", "001", "010", "010", "010"],
  "8": ["111", "101", "111", "101", "111"],
  "9": ["111", "101", "111", "001", "111"],
  "+": ["000", "010", "111", "010", "000"],
};

const SIZE = 32;

/** A red disc with the count in white, as a Windows taskbar overlay. */
function renderBadge(count: number): NativeImage {
  const text = count > 9 ? "9+" : String(count);
  const px = Buffer.alloc(SIZE * SIZE * 4); // BGRA
  const set = (x: number, y: number, b: number, g: number, r: number, a: number) => {
    const i = (y * SIZE + x) * 4;
    px[i] = b;
    px[i + 1] = g;
    px[i + 2] = r;
    px[i + 3] = a;
  };

  const c = (SIZE - 1) / 2;
  for (let y = 0; y < SIZE; y++) {
    for (let x = 0; x < SIZE; x++) {
      const d = Math.hypot(x - c, y - c);
      // One pixel of soft edge so the disc doesn't look jagged.
      const alpha = Math.max(0, Math.min(1, c + 0.5 - d));
      if (alpha > 0) set(x, y, 0x45, 0x3c, 0xed, Math.round(alpha * 255));
    }
  }

  const scale = text.length > 1 ? 3 : 4;
  const gap = scale;
  const width = text.length * 3 * scale + (text.length - 1) * gap;
  const left = Math.round((SIZE - width) / 2);
  const top = Math.round((SIZE - 5 * scale) / 2);
  [...text].forEach((ch, idx) => {
    const glyph = GLYPHS[ch];
    const ox = left + idx * (3 * scale + gap);
    glyph.forEach((row, gy) =>
      [...row].forEach((bit, gx) => {
        if (bit !== "1") return;
        for (let dy = 0; dy < scale; dy++)
          for (let dx = 0; dx < scale; dx++) set(ox + gx * scale + dx, top + gy * scale + dy, 255, 255, 255, 255);
      }),
    );
  });

  return nativeImage.createFromBitmap(px, { width: SIZE, height: SIZE });
}

let last = 0;

export function applyUnreadCount(window: BrowserWindow, count: number): void {
  if (count === last) return;
  const grew = count > last;
  last = count;

  if (process.platform === "win32") {
    window.setOverlayIcon(count > 0 ? renderBadge(count) : null, count > 0 ? `${count} unread` : "");
  } else {
    // Unity-style launchers (and macOS later) show this; elsewhere it's a no-op.
    app.setBadgeCount(count);
  }

  if (grew && !window.isFocused()) window.flashFrame(true);
}
