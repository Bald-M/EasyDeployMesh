export const desktopTargets = Object.freeze({
  "macos-arm64": {
    platform: "macos",
    rustTarget: "aarch64-apple-darwin",
    bundles: "dmg",
    extension: ".dmg",
    artifactLabel: "macOS-Apple-Silicon",
  },
  "macos-x64": {
    platform: "macos",
    rustTarget: "x86_64-apple-darwin",
    bundles: "dmg",
    extension: ".dmg",
    artifactLabel: "macOS-Intel",
  },
  "windows-arm64": {
    platform: "windows",
    rustTarget: "aarch64-pc-windows-msvc",
    bundles: "nsis",
    extension: ".exe",
    artifactLabel: "Windows-ARM64",
  },
  "windows-x86": {
    platform: "windows",
    rustTarget: "i686-pc-windows-msvc",
    bundles: "nsis",
    extension: ".exe",
    artifactLabel: "Windows-x86",
  },
  "windows-x64": {
    platform: "windows",
    rustTarget: "x86_64-pc-windows-msvc",
    bundles: "nsis",
    extension: ".exe",
    artifactLabel: "Windows-x64",
  },
  "linux-arm64": {
    platform: "linux",
    rustTarget: "aarch64-unknown-linux-gnu",
    bundles: "appimage",
    extension: ".AppImage",
    artifactLabel: "Linux-ARM64",
  },
  "linux-x64": {
    platform: "linux",
    rustTarget: "x86_64-unknown-linux-gnu",
    bundles: "appimage",
    extension: ".AppImage",
    artifactLabel: "Linux-x64",
  },
});

export function desktopTarget(name) {
  const target = desktopTargets[name];
  if (!target) {
    throw new Error(
      `Unknown desktop target ${JSON.stringify(name)}. Expected one of: ${Object.keys(desktopTargets).join(", ")}`,
    );
  }
  return target;
}
