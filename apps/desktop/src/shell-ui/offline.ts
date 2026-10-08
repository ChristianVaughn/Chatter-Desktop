export {};

const params = new URLSearchParams(location.search);
const detail = document.getElementById("detail") as HTMLParagraphElement;
const retry = document.getElementById("retry") as HTMLButtonElement;
const change = document.getElementById("change") as HTMLButtonElement;

const url = params.get("url");
const error = params.get("error");
let host = "";
try {
  host = url ? new URL(url).host : "";
} catch {
  // Leave it out of the message.
}
detail.textContent = [host && `${host} didn't respond.`, error].filter(Boolean).join(" ");

retry.addEventListener("click", () => void window.shellApi.retryServer());
change.addEventListener("click", () => void window.shellApi.changeServer());

// Come back by ourselves once the network does.
window.addEventListener("online", () => void window.shellApi.retryServer());
