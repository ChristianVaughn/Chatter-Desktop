export {};

// Captures one key or mouse button in this window only — nothing system-wide —
// and hands its DOM code to the main process.
const done = (code: string | null) => void window.shellApi.keybindDone(code);

window.addEventListener("keydown", (event) => {
  event.preventDefault();
  if (event.code === "Escape") return done(null);
  if (event.code) done(event.code);
});

window.addEventListener("mousedown", (event) => {
  // 1 middle, 3 back (Mouse 4), 4 forward (Mouse 5). Left and right are left
  // alone so the window can still be used.
  const code = { 1: "Mouse3", 3: "Mouse4", 4: "Mouse5" }[event.button];
  if (code) {
    event.preventDefault();
    done(code);
  }
});
window.addEventListener("auxclick", (event) => event.preventDefault());
window.addEventListener("contextmenu", (event) => event.preventDefault());

document.getElementById("cancel")!.addEventListener("click", () => done(null));
document.getElementById("capture")!.focus();
