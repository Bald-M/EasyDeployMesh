import { createHash } from "node:crypto";
import { chmod, copyFile, mkdir, readFile, stat, writeFile } from "node:fs/promises";
import { arch, platform } from "node:os";
import { dirname, resolve } from "node:path";
import { spawn } from "node:child_process";

export const WIMLIB_VERSION = "1.14.5";
export const WIMLIB_SOURCE_SHA256 = "84221a3abd5b91228f15f8e6065c335a336237b5738197b75bf419eea561a194";
const binarySha256 = {
  "aarch64-apple-darwin": "f5b3a8afc214cd48c96fa1046915d7ea0e21ba1138b2cedac305208729f0ccd5",
  "x86_64-apple-darwin": "3bce822388fc54593a64aeff45b03a0beeedc18e45230268526608375e4d318c"
};
const sourceUrl = `https://wimlib.net/downloads/wimlib-${WIMLIB_VERSION}.tar.gz`;

export function rustTarget(requested = "current") {
  if (requested === "macos-arm64") return "aarch64-apple-darwin";
  if (requested === "macos-x64") return "x86_64-apple-darwin";
  if (requested !== "current") throw new Error(`unsupported wimlib target: ${requested}`);
  if (platform() !== "darwin") throw new Error("wimlib staging is only required for macOS");
  if (arch() === "arm64") return "aarch64-apple-darwin";
  if (arch() === "x64") return "x86_64-apple-darwin";
  throw new Error(`unsupported macOS architecture: ${arch()}`);
}

function run(command, args, options = {}) {
  return new Promise((resolvePromise, reject) => {
    const child = spawn(command, args, { stdio: "inherit", ...options });
    child.on("error", reject);
    child.on("exit", (code, signal) => code === 0
      ? resolvePromise()
      : reject(new Error(`${command} exited with ${signal ? `signal ${signal}` : `code ${code}`}`)));
  });
}

async function sha256(path) {
  return createHash("sha256").update(await readFile(path)).digest("hex");
}

async function ensureSource(cacheRoot) {
  const archive = resolve(cacheRoot, `wimlib-${WIMLIB_VERSION}.tar.gz`);
  await mkdir(cacheRoot, { recursive: true });
  let valid = false;
  try { valid = await sha256(archive) === WIMLIB_SOURCE_SHA256; } catch {}
  if (!valid) await run("curl", ["-fL", sourceUrl, "-o", archive]);
  if (await sha256(archive) !== WIMLIB_SOURCE_SHA256) throw new Error("wimlib source checksum mismatch");
  const source = resolve(cacheRoot, `wimlib-${WIMLIB_VERSION}`);
  try { await stat(resolve(source, "configure")); } catch { await run("tar", ["-xzf", archive, "-C", cacheRoot], { env: { ...process.env, LC_ALL: "en_US.UTF-8", LANG: "en_US.UTF-8" } }); }
  return source;
}

async function main() {
  const target = rustTarget(process.argv[2]);
  const destination = resolve(`apps/desktop/src-tauri/binaries/wimlib-imagex-${target}`);
  try {
    if (await sha256(destination) === binarySha256[target]) {
      console.log(`Bundled wimlib ${WIMLIB_VERSION} for ${target} is ready`);
      return;
    }
  } catch {}
  const cacheRoot = resolve("target/wimlib-source");
  const source = await ensureSource(cacheRoot);
  const build = resolve(`target/wimlib-build/${target}`);
  await mkdir(build, { recursive: true });
  const configureArgs = ["--disable-shared", "--enable-static", "--without-ntfs-3g", "--without-fuse"];
  const environment = { ...process.env, LC_ALL: "en_US.UTF-8", LANG: "en_US.UTF-8" };
  const pkgConfigShim = resolve(cacheRoot, "pkg-config-disabled");
  await writeFile(pkgConfigShim, '#!/bin/sh\n[ "$1" = "--atleast-pkgconfig-version" ] && exit 0\nexit 1\n');
  await chmod(pkgConfigShim, 0o755);
  environment.PKG_CONFIG = pkgConfigShim;
  if (target === "x86_64-apple-darwin" && arch() === "arm64") {
    configureArgs.unshift("--host=x86_64-apple-darwin");
    environment.CC = "clang -arch x86_64";
  }
  await run(resolve(source, "configure"), configureArgs, { cwd: build, env: environment });
  await run("make", ["-j4"], { cwd: build, env: environment });
  const built = resolve(build, "wimlib-imagex");
  await mkdir(dirname(destination), { recursive: true });
  await copyFile(built, destination);
  const digest = await sha256(destination);
  if (digest !== binarySha256[target]) throw new Error(`wimlib binary checksum mismatch for ${target}: ${digest}`);
  await writeFile(`${destination}.sha256`, `${digest}  wimlib-imagex-${target}\n`);
  await copyFile(resolve(source, "COPYING.GPLv3"), resolve("apps/desktop/src-tauri/binaries/wimlib-COPYING.GPLv3"));
  console.log(`Staged wimlib ${WIMLIB_VERSION} for ${target}: ${digest}`);
}

if (process.argv[1] && import.meta.url === new URL(`file://${resolve(process.argv[1])}`).href) await main();
