import type { AudioChoice, CaptureSource, PickerData } from "../shared/ipc";

const grid = document.getElementById("grid") as HTMLDivElement;
const share = document.getElementById("share") as HTMLButtonElement;
const cancel = document.getElementById("cancel") as HTMLButtonElement;
const audioRow = document.getElementById("audio-row") as HTMLDivElement;
const audioSelect = document.getElementById("audio") as HTMLSelectElement;
const tabs = [...document.querySelectorAll<HTMLButtonElement>("[role=tab]")];

let sources: CaptureSource[] = [];
let audio: PickerData["audio"] = null;
let kind: CaptureSource["kind"] = "screen";
let selected: string | null = null;
/** Set once the person picks audio themselves; until then it follows the source. */
let audioTouched = false;

// Option values: "none", "window", "system", or "app:<pid>".
function audioOptions(): { value: string; label: string }[] {
  if (!audio) return [];
  const options = [{ value: "none", label: "No audio" }];
  const source = sources.find((s) => s.id === selected);
  if (audio.windowApp && source?.kind === "window") options.push({ value: "window", label: "This window's app" });
  if (audio.allExcept) options.push({ value: "system", label: "Everything except Chatter" });
  if (audio.perApp) for (const app of audio.apps) options.push({ value: `app:${app.pid}`, label: app.name });
  return options;
}

function renderAudio(): void {
  const options = audioOptions();
  audioRow.hidden = options.length <= 1;
  const previous = audioSelect.value;
  audioSelect.replaceChildren(
    ...options.map((o) => {
      const el = document.createElement("option");
      el.value = o.value;
      el.textContent = o.label;
      return el;
    }),
  );
  const values = options.map((o) => o.value);
  if (audioTouched && values.includes(previous)) {
    audioSelect.value = previous;
  } else {
    // A window shares its own app's sound; a whole screen shares everything
    // but the call — what Discord does.
    const source = sources.find((s) => s.id === selected);
    const preferred = source?.kind === "window" ? "window" : "system";
    audioSelect.value = values.includes(preferred) ? preferred : "none";
  }
}

function chosenAudio(): AudioChoice {
  const value = audioRow.hidden ? "none" : audioSelect.value;
  if (value.startsWith("app:")) return { kind: "app", pid: Number(value.slice(4)) };
  if (value === "window" || value === "system") return { kind: value };
  return { kind: "none" };
}

function choose(id: string | null): void {
  void window.shellApi.pickerChoose(id, id ? chosenAudio() : undefined);
}

function select(id: string): void {
  selected = id;
  share.disabled = false;
  render();
  renderAudio();
}

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
    button.addEventListener("click", () => select(source.id));
    button.addEventListener("dblclick", () => {
      select(source.id);
      choose(source.id);
    });
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

audioSelect.addEventListener("change", () => (audioTouched = true));
share.addEventListener("click", () => {
  if (selected) choose(selected);
});
cancel.addEventListener("click", () => choose(null));
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape") choose(null);
});

void window.shellApi.pickerGetSources().then((data) => {
  sources = data.sources;
  audio = data.audio;
  // With a single screen there's nothing to choose between on that tab, so
  // preselect it; Share is then one click.
  const screens = sources.filter((s) => s.kind === "screen");
  if (screens.length === 1) select(screens[0].id);
  else render();
  renderAudio();
});
