import type { DesktopPrefsView } from "../shared/ipc";

const toggles = ["startWithSystem", "startMinimized", "shareGameActivity", "autoUpdate"] as const;
const games = document.getElementById("games") as HTMLUListElement;
const running = document.getElementById("running") as HTMLSelectElement;
const refresh = document.getElementById("refresh") as HTMLButtonElement;
const version = document.getElementById("version") as HTMLParagraphElement;

let prefs: DesktopPrefsView;

function renderGames(): void {
  games.replaceChildren();
  for (const game of prefs.extraGames) {
    const li = document.createElement("li");
    const name = document.createElement("span");
    name.textContent = `${game.name} (${game.exe})`;
    const remove = document.createElement("button");
    remove.type = "button";
    remove.textContent = "Remove";
    remove.addEventListener("click", () => {
      void save({ extraGames: prefs.extraGames.filter((g) => g.exe !== game.exe) });
    });
    li.append(name, remove);
    games.append(li);
  }
}

async function save(update: Partial<DesktopPrefsView>): Promise<void> {
  prefs = await window.shellApi.settingsSet(update);
  render();
}

function render(): void {
  for (const key of toggles) (document.getElementById(key) as HTMLInputElement).checked = prefs[key];
  (document.getElementById("startMinimized") as HTMLInputElement).disabled = !prefs.startWithSystem;
  renderGames();
  version.textContent = `Chatter ${prefs.appVersion}`;
}

async function loadRunning(): Promise<void> {
  const list = await window.shellApi.runningApps();
  running.replaceChildren(new Option("Add a running program…", ""));
  for (const app of list) running.append(new Option(`${app.name} (${app.exe})`, JSON.stringify(app)));
}

for (const key of toggles) {
  document.getElementById(key)!.addEventListener("change", (e) => {
    void save({ [key]: (e.target as HTMLInputElement).checked });
  });
}

running.addEventListener("change", () => {
  if (!running.value) return;
  const app = JSON.parse(running.value) as { exe: string; name: string };
  running.value = "";
  if (prefs.extraGames.some((g) => g.exe === app.exe)) return;
  void save({ extraGames: [...prefs.extraGames, app] });
});
refresh.addEventListener("click", () => void loadRunning());

void window.shellApi.settingsGet().then((p) => {
  prefs = p;
  render();
});
void loadRunning();
