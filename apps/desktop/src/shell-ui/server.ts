export {};

const form = document.getElementById("form") as HTMLFormElement;
const input = document.getElementById("url") as HTMLInputElement;
const error = document.getElementById("error") as HTMLDivElement;
const submit = document.getElementById("submit") as HTMLButtonElement;
const cancel = document.getElementById("cancel") as HTMLButtonElement;

const changing = new URLSearchParams(location.search).has("change");

void window.shellApi.getServer().then((current) => {
  if (!current) return;
  input.value = current;
  input.select();
  // Changing servers can be backed out of; first launch can't.
  if (changing) cancel.hidden = false;
});

cancel.addEventListener("click", () => void window.shellApi.retryServer());

form.addEventListener("submit", async (event) => {
  event.preventDefault();
  error.textContent = "";
  submit.disabled = true;
  submit.textContent = "Checking…";
  try {
    const result = await window.shellApi.probeServer(input.value);
    if (result.ok && result.origin) {
      await window.shellApi.setServer(result.origin);
      return;
    }
    error.textContent = result.error ?? "Couldn't connect.";
  } finally {
    submit.disabled = false;
    submit.textContent = "Connect";
  }
});
