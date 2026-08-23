import { spawn } from "node:child_process";
import process from "node:process";
import { desktopTarget } from "./desktop-targets.mjs";

const targetName = process.argv[2];
const target = desktopTarget(targetName);

function run(command, args, extraEnvironment = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      stdio: "inherit",
      env: { ...process.env, ...extraEnvironment },
    });
    child.on("error", reject);
    child.on("exit", (code, signal) => {
      if (code === 0) {
        resolve();
        return;
      }
      reject(new Error(`${command} exited with ${signal ? `signal ${signal}` : `code ${code}`}`));
    });
  });
}

const windowsAgentTarget = "x86_64-pc-windows-msvc";
const windowsEnvironment = {
  RUSTFLAGS: "-C target-feature=+crt-static",
  XWIN_ARCH: "x86_64",
};
if (process.platform === "win32") {
  await run("cargo", ["build", "-p", "easydeploymesh-agent", "--release", "--target", windowsAgentTarget], windowsEnvironment);
} else {
  await run("cargo", ["xwin", "build", "-p", "easydeploymesh-agent", "--release", "--target", windowsAgentTarget], windowsEnvironment);
}

await run("node", ["scripts/stage-agent.mjs"]);

const tauriArguments = [
  "--filter",
  "@easydeploymesh/desktop",
  "tauri",
  "build",
  "--target",
  target.rustTarget,
  "--bundles",
  target.bundles,
];
if (target.platform === "windows" && process.platform !== "win32") {
  tauriArguments.push("--runner", "cargo-xwin");
}
const tauriEnvironment = { LC_ALL: "en_US.UTF-8", LANG: "en_US.UTF-8" };
if (target.platform === "windows" && process.platform !== "win32") {
  tauriEnvironment.XWIN_ARCH = target.xwinArch;
}
await run("pnpm", tauriArguments, tauriEnvironment);
await run("node", ["scripts/collect-installer.mjs", targetName]);
