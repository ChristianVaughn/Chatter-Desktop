import type { ShellApi } from "../shared/ipc";

declare global {
  interface Window {
    shellApi: ShellApi;
  }
}
