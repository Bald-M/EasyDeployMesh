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
    xwinArch: "aarch64",
    bundles: "nsis",
    extension: ".exe",
    artifactLabel: "Windows-ARM64",
  },
  "windows-x86": {
    platform: "windows",
    rustTarget: "i686-pc-windows-msvc",
    xwinArch: "x86",
    bundles: "nsis",
    extension: ".exe",
    artifactLabel: "Windows-x86",
  },
  "windows-x64": {
    platform: "windows",
    rustTarget: "x86_64-pc-windows-msvc",
    xwinArch: "x86_64",
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

const hostPlatforms = Object.freeze({
  darwin: ["macos", "windows"],
  linux: ["linux", "windows"],
  win32: ["windows"],
});

const platformAliases = Object.freeze({
  mac: "macos",
  macos: "macos",
  windows: "windows",
  linux: "linux",
});

export function desktopBuildTargets(hostPlatform, selectors = []) {
  const supportedPlatforms = hostPlatforms[hostPlatform];
  if (!supportedPlatforms) {
    throw new Error(`Unsupported build host ${JSON.stringify(hostPlatform)}`);
  }

  const requested = selectors.length === 0 || selectors.includes("native")
    ? supportedPlatforms
    : selectors;
  const names = [];
  for (const selector of requested) {
    const platform = platformAliases[selector];
    const matches = platform
      ? Object.entries(desktopTargets)
          .filter(([, target]) => target.platform === platform)
          .map(([name]) => name)
      : desktopTargets[selector]
        ? [selector]
        : [];
    if (matches.length === 0) {
      throw new Error(
        `Unknown build selector ${JSON.stringify(selector)}. Expected native, mac, windows, linux, or a desktop target.`,
      );
    }
    for (const name of matches) {
      if (!supportedPlatforms.includes(desktopTargets[name].platform)) {
        throw new Error(
          `${name} cannot be built on ${hostPlatform}; ${desktopTargets[name].platform} installers require their native host.`,
        );
      }
      if (!names.includes(name)) {
        names.push(name);
      }
    }
  }
  return names;
}

export function desktopBuildSelectors(argv) {
  return argv.filter((argument) => argument !== "--");
}
