import { spawn } from "node:child_process";
import process from "node:process";
import { desktopBuildSelectors, desktopBuildTargets } from "./desktop-targets.mjs";

function run(command, args) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { stdio: "inherit", env: process.env });
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

let targets;
try {
  targets = desktopBuildTargets(
    process.platform,
    desktopBuildSelectors(process.argv.slice(2)),
  );
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exit(1);
}
console.log(`Building installers: ${targets.join(", ")}`);

for (const target of targets) {
  await run("node", ["scripts/build-desktop.mjs", target]);
}

console.log(`Built ${targets.length} installer target${targets.length === 1 ? "" : "s"} in release/.`);
