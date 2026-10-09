import { resolve } from "node:path";
import { defineConfig, externalizeDepsPlugin } from "electron-vite";

export default defineConfig({
  main: {
    plugins: [externalizeDepsPlugin()],
  },
  preload: {
    plugins: [externalizeDepsPlugin()],
    build: {
      rollupOptions: {
        // One preload for every window. It asks the main process what the
        // page is (the user's Chatter server, or one of the shell's own pages)
        // and exposes only that role's API. Sandboxed preloads can't require
        // sibling chunks, so it must build to a single CommonJS file.
        input: { index: resolve(__dirname, "src/preload/index.ts") },
        output: { format: "cjs", entryFileNames: "[name].js" },
      },
    },
  },
  renderer: {
    root: resolve(__dirname, "src/shell-ui"),
    build: {
      rollupOptions: {
        input: {
          server: resolve(__dirname, "src/shell-ui/server.html"),
          picker: resolve(__dirname, "src/shell-ui/picker.html"),
          offline: resolve(__dirname, "src/shell-ui/offline.html"),
          keybind: resolve(__dirname, "src/shell-ui/keybind.html"),
          settings: resolve(__dirname, "src/shell-ui/settings.html"),
        },
      },
    },
  },
});
