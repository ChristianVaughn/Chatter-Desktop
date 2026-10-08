// Runs a local Chatter server for development, from a sibling checkout of the
// Chatter repo (../Chatter) or the directory in CHATTER_DIR.
import { spawn } from "node:child_process";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const cwd = resolve(process.env.CHATTER_DIR ?? resolve(here, "../../Chatter"));
const child = spawn("cargo", ["run", ...process.argv.slice(2)], { cwd, stdio: "inherit", shell: process.platform === "win32" });
child.on("exit", (code) => process.exit(code ?? 1));
for (const signal of ["SIGINT", "SIGTERM"]) process.on(signal, () => child.kill(signal));
