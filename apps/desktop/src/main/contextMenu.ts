import { Menu, clipboard, shell, type MenuItemConstructorOptions, type WebContents } from "electron";
import { isWebUrl } from "./origin";

/**
 * Electron has no default right-click menu. The Chatter client draws its own
 * on messages and cancels the event there; this covers what's left: text
 * fields (with spelling fixes), selected text, and links.
 */
export function attachContextMenu(contents: WebContents): void {
  contents.on("context-menu", (_event, params) => {
    const items: MenuItemConstructorOptions[] = [];

    if (params.misspelledWord) {
      for (const word of params.dictionarySuggestions.slice(0, 5)) {
        items.push({ label: word, click: () => contents.replaceMisspelling(word) });
      }
      items.push({
        label: "Add to dictionary",
        click: () => contents.session.addWordToSpellCheckerDictionary(params.misspelledWord),
      });
      items.push({ type: "separator" });
    }

    if (params.linkURL && isWebUrl(params.linkURL)) {
      items.push(
        { label: "Open link in browser", click: () => void shell.openExternal(params.linkURL) },
        { label: "Copy link", click: () => clipboard.writeText(params.linkURL) },
        { type: "separator" },
      );
    }

    if (params.isEditable) {
      items.push(
        { role: "undo", enabled: params.editFlags.canUndo },
        { role: "redo", enabled: params.editFlags.canRedo },
        { type: "separator" },
        { role: "cut", enabled: params.editFlags.canCut },
        { role: "copy", enabled: params.editFlags.canCopy },
        { role: "paste", enabled: params.editFlags.canPaste },
        { role: "selectAll" },
      );
    } else if (params.selectionText.trim()) {
      items.push({ role: "copy" });
    }

    while (items.at(-1)?.type === "separator") items.pop();
    if (items.length) Menu.buildFromTemplate(items).popup();
  });
}
