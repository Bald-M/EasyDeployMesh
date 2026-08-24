import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const verifierPath = new URL("./verify-winpe-package.ps1", import.meta.url);
const collectorPath = new URL("./collect-winpe-runtime.cmd", import.meta.url);
const pxeSourcePath = new URL("../crates/service/src/pxe.rs", import.meta.url);
const [verifier, collector, pxeSource] = await Promise.all([
  readFile(verifierPath, "utf8"),
  readFile(collectorPath, "utf8"),
  readFile(pxeSourcePath, "utf8"),
]);

test("package verifier uses the WIM header boot index and a discard-only mount", () => {
  assert.match(verifier, /MSWIM/);
  assert.match(verifier, /ToUInt32\(\$header, 0x2c\)/);
  assert.match(verifier, /ToUInt32\(\$header, 0x78\)/);
  assert.match(verifier, /\/Mount-Image/);
  assert.match(verifier, /\/Index:\$\(\$wimHeader\.BootIndex\)/);
  assert.match(verifier, /\/ReadOnly/);
  assert.match(verifier, /finally\s*\{/);
  assert.match(verifier, /\/Unmount-Image[\s\S]*\/Discard/);
  assert.doesNotMatch(verifier, /\/Commit/);
});

test("package verifier checks every injected startup-chain artifact", () => {
  for (const artifact of [
    "easydeploymesh-agent.exe",
    "easydeploymesh-shell.exe",
    "collect-winpe-runtime.cmd",
    "easydeploymesh-bootstrap.json",
    "easydeploymesh-agent.sha256",
    "easydeploymesh-runtime.sha256",
    "shell-hook.enabled",
    "startnet.cmd",
    "startnet.easydeploymesh-original.cmd",
    "winpeshl.ini",
    "easydeploymesh-original-shell.cmd",
  ]) {
    assert.match(verifier, new RegExp(artifact.replaceAll(".", "\\."), "i"));
  }
  assert.match(verifier, /Get-FileHash[\s\S]*SHA256/);
  const verifierRevision = verifier.match(/easydeploymesh-winpe-runtime-layout-v\d+/)?.[0];
  const runtimeRevision = pxeSource.match(/easydeploymesh-winpe-runtime-layout-v\d+/)?.[0];
  assert.ok(verifierRevision, "verifier runtime layout revision is missing");
  assert.equal(verifierRevision, runtimeRevision);
  assert.match(verifier, /Embedded runtime layout hash/);
  assert.match(verifier, /Original EasyU shell preservation/);
});

test("bootstrap diagnostics redact enrollment credentials", () => {
  assert.match(verifier, /\[present, redacted\]/i);
  assert.match(collector, /\[PRESENT, REDACTED\]/i);
  assert.match(collector, /easydeploymesh_enroll_/i);
  assert.match(collector, /findstr \/V \/I[\s\S]*enrollmentToken/);
  assert.doesNotMatch(collector, /type\s+[^\r\n]*easydeploymesh-bootstrap\.json/i);
});

test("runtime collector only asks diskpart for inventory", () => {
  const diskpartCommands = [...collector.matchAll(/echo ([^\r\n]+)\r?\n/g)]
    .map((match) => match[1].trim().toLowerCase())
    .filter((line) => line.startsWith("list "));
  assert.deepEqual(diskpartCommands, ["list disk", "list volume"]);
  assert.doesNotMatch(
    collector,
    /(?:ghost|eix|easyimagex)(?:32|64|2|exp)?\.exe[\s\"]+(?:-|\/)?(?:restore|clone|sure|batch|src|dst)\b/i,
  );
});

test("runtime collector does not discover or execute external imaging tools", () => {
  assert.doesNotMatch(collector, /ghost(?:32|64)?\.exe/i);
  assert.doesNotMatch(collector, /easyimagex|eix2?\.exe/i);
  assert.match(collector, /wpeutil\.exe" UpdateBootInfo/);
  assert.match(collector, /easydeploymesh-agent\.exe" --version/);
  assert.match(
    collector,
    /easydeploymesh-agent\.exe" --bootstrap "X:\\EasyDeployMesh\\easydeploymesh-bootstrap\.json" --health-check/,
  );
  assert.doesNotMatch(collector, /easydeploymesh-agent\.exe"[^\r\n]*--once/i);
  assert.match(collector, /ipconfig \/all/);
  assert.match(collector, /route print/);
  assert.match(collector, /diskpart \/s/);

  assert.doesNotMatch(collector, /-[?]|\/[?]/);
});

test("runtime collector emits locale-independent lifecycle markers", () => {
  for (const marker of [
    "EASYDEPLOYMESH_DIAG_V1^|agent.binary^|present",
    "EASYDEPLOYMESH_DIAG_V1^|agent.binary^|missing",
    "EASYDEPLOYMESH_DIAG_V1^|agent.process^|running",
    "EASYDEPLOYMESH_DIAG_V1^|agent.process^|absent",
    "EASYDEPLOYMESH_DIAG_V1^|agent.version_probe^|ok^|exit=0",
    "EASYDEPLOYMESH_DIAG_V1^|agent.version_probe^|failed^|exit=",
    "EASYDEPLOYMESH_DIAG_V1^|bootstrap.discovery^|none",
    "EASYDEPLOYMESH_DIAG_V1^|bootstrap.authoritative^|present",
    "EASYDEPLOYMESH_DIAG_V1^|bootstrap.authoritative^|missing",
    "EASYDEPLOYMESH_DIAG_V1^|control.health^|not_run^|reason=missing_runtime_input",
    "EASYDEPLOYMESH_DIAG_V1^|control.health_probe^|exit=",
    "EASYDEPLOYMESH_DIAG_V1^|agent.log^|present",
    "EASYDEPLOYMESH_DIAG_V1^|agent.log^|empty",
    "EASYDEPLOYMESH_DIAG_V1^|agent.log^|missing",
    "EASYDEPLOYMESH_DIAG_V1^|collector.complete^|ok",
  ]) {
    assert.ok(collector.includes(marker), `missing collector marker: ${marker}`);
  }
  assert.match(collector, /Server: \[unavailable without a safe JSON parser\]/);
  assert.doesNotMatch(collector, /echo Server: !EASYDEPLOYMESH_SERVER!/);
});

test("missing WIM deployment tools are informational only", () => {
  assert.match(verifier, /WIM Ghost\/EIX candidates: none found \(informational; Agent checks are unaffected\)/);
  assert.match(verifier, /WIM tool candidate \(not executed\)/);
  assert.doesNotMatch(verifier, /Add-Check[^\r\n]+Ghost\/EIX/i);
});
