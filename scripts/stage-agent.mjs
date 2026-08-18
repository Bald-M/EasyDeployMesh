import { copyFile, mkdir } from "node:fs/promises";
import { dirname, resolve } from "node:path";

// The bundled Agent runs inside Windows PE; it is not a sidecar for the host OS.
// Every desktop package therefore carries the same statically linked Windows x64 binary.
const sourcePath = "target/x86_64-pc-windows-msvc/release/easydeploymesh-agent.exe";
const destinationPath =
  "apps/desktop/src-tauri/binaries/easydeploymesh-agent-x86_64-pc-windows-msvc.exe";
const source = resolve(sourcePath);
const destination = resolve(destinationPath);
await mkdir(dirname(destination), { recursive: true });
await copyFile(source, destination);
console.log(`Staged ${destination}`);
