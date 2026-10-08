import type { CaptureSource } from "../shared/ipc";

const grid = document.getElementById("grid") as HTMLDivElement;
const share = document.getElementById("share") as HTMLButtonElement;
const cancel = document.getElementById("cancel") as HTMLButtonElement;
const tabs = [...document.querySelectorAll<HTMLButtonElement>("[role=tab]")];

let sources: CaptureSource[] = [];
let kind: CaptureSource["kind"] = "screen";
let selected: string | null = null;

function render(): void {
  grid.replaceChildren();
  const shown = sources.filter((s) => s.kind === kind);
  if (!shown.length) {
    const empty = document.createElement("div");
    empty.className = "empty";
    empty.textContent = kind === "screen" ? "No screens found." : "No windows found.";
    grid.append(empty);
  }
  for (const source of shown) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "source";
    button.setAttribute("aria-pressed", String(source.id === selected));

    const thumb = document.createElement("img");
    thumb.className = "thumb";
    thumb.src = source.thumbnail;
    thumb.alt = "";

    const name = document.createElement("div");
    name.className = "name";
    if (source.appIcon) {
      const appIcon = document.createElement("img");
      appIcon.src = source.appIcon;
      appIcon.alt = "";
      name.append(appIcon);
    }
    const label = document.createElement("span");
    label.textContent = source.name;
    name.append(label);

    button.append(thumb, name);
    button.addEventListener("click", () => {
      selected = source.id;
      share.disabled = false;
      render();
    });
    button.addEventListener("dblclick", () => void window.shellApi.pickerChoose(source.id));
    grid.append(button);
  }
}

for (const tab of tabs) {
  tab.addEventListener("click", () => {
    kind = tab.dataset.kind as CaptureSource["kind"];
    for (const t of tabs) t.setAttribute("aria-selected", String(t === tab));
    render();
  });
}

share.addEventListener("click", () => {
  if (selected) void window.shellApi.pickerChoose(selected);
});
cancel.addEventListener("click", () => void window.shellApi.pickerChoose(null));
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape") void window.shellApi.pickerChoose(null);
});

void window.shellApi.pickerGetSources().then((list) => {
  sources = list;
  // With a single screen there's nothing to choose between on that tab, so
  // preselect it; Share is then one click.
  const screens = list.filter((s) => s.kind === "screen");
  if (screens.length === 1) {
    selected = screens[0].id;
    share.disabled = false;
  }
  render();
});
