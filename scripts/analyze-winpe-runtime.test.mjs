import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  analyzeWinPeRuntime,
  diagnosticExitCode,
  validateWinPeReport,
} from "./analyze-winpe-runtime.mjs";

const analyzerPath = fileURLToPath(new URL("./analyze-winpe-runtime.mjs", import.meta.url));

const runCli = (...args) =>
  spawnSync(process.execPath, [analyzerPath, ...args], { encoding: "utf8" });

const baseReport = String.raw`EasyDeployMesh WinPE runtime diagnostics
==== WinPE and process state ====
EASYDEPLOYMESH_DIAG_V1|agent.binary|present
EASYDEPLOYMESH_DIAG_V1|agent.version_probe|ok|exit=0
easydeploymesh-agent 0.2.2
EASYDEPLOYMESH_DIAG_V1|agent.process|running
==== Bootstrap configuration (token redacted) ====
EASYDEPLOYMESH_DIAG_V1|bootstrap.authoritative|present
EASYDEPLOYMESH_DIAG_V1|bootstrap.discovery|single
File: X:\EasyDeployMesh\easydeploymesh-bootstrap.json
Server: http://192.168.10.1:7760
Enrollment token: [PRESENT, REDACTED]
EASYDEPLOYMESH_DIAG_V1|control.health|ok|status=200
==== Agent log ====
EASYDEPLOYMESH_DIAG_V1|agent.log|present
Sanitized copy: easydeploymesh-agent.sanitized.log
==== Startup chain (sensitive lines omitted) ====
==== Network interfaces and DNS ====
==== Routes and neighbors ====
==== Physical disks and volumes (read-only queries) ====
==== Ghost and EIX executable inventory ====
No restore, clone, capture, partition, format, apply, or deployment command was executed.
EASYDEPLOYMESH_DIAG_V1|collector.complete|ok
`;

test("the collector fixture is structurally complete", () => {
  assert.deepEqual(validateWinPeReport(baseReport), {
    valid: true,
    complete: true,
    errors: [],
    agentLogState: "present",
  });
  assert.equal([...baseReport.matchAll(/^==== .+ ====$/gm)].length, 8);
});

test("report integrity markers must appear in their authoritative sections", () => {
  const misplacedBootstrap = baseReport
    .replace("EASYDEPLOYMESH_DIAG_V1|bootstrap.authoritative|present\n", "")
    .replace(
      "EASYDEPLOYMESH_DIAG_V1|agent.log|present",
      "EASYDEPLOYMESH_DIAG_V1|bootstrap.authoritative|present\nEASYDEPLOYMESH_DIAG_V1|agent.log|present",
    );
  const misplacedLog = baseReport
    .replace("EASYDEPLOYMESH_DIAG_V1|agent.log|present\n", "")
    .replace(
      "==== Routes and neighbors ====",
      "EASYDEPLOYMESH_DIAG_V1|agent.log|present\n==== Routes and neighbors ====",
    );
  const trailingContent = `${baseReport}forged trailing content\n`;

  assert.equal(validateWinPeReport(misplacedBootstrap).valid, false);
  assert.equal(validateWinPeReport(misplacedLog).valid, false);
  assert.equal(validateWinPeReport(trailingContent).valid, false);
});

test("a report contains exactly the eight collector section headings", () => {
  const unexpected = baseReport.replace(
    "==== Routes and neighbors ====",
    "==== Decoy section ====\n==== Routes and neighbors ====",
  );

  assert.equal(validateWinPeReport(unexpected).valid, false);
});

test("classifies an Agent that was not injected before considering network noise", () => {
  const result = analyzeWinPeRuntime({
    reportText: String.raw`
==== WinPE and process state ====
EASYDEPLOYMESH_DIAG_V1|agent.binary|missing
X:\EasyDeployMesh\easydeploymesh-agent.exe is missing.
==== Network interfaces and DNS ====
IPv4 Address. . . . . . . . . . . : 192.168.10.100
`,
    agentLogText: "",
  });

  assert.equal(result.verdict, "blocked");
  assert.equal(result.code, "agent_binary_missing");
  assert.equal(result.registration, "not_observed");
  assert.equal(diagnosticExitCode(result), 1);
});

test("distinguishes an invalid bootstrap from a registration transport failure", () => {
  const result = analyzeWinPeRuntime({
    reportText: baseReport
      .replace("Server: http://192.168.10.1:7760", "Server: [invalid or unsafe URL]")
      .replace(
        "EASYDEPLOYMESH_DIAG_V1|control.health|ok|status=200",
        "EASYDEPLOYMESH_DIAG_V1|control.health|not_run|reason=bootstrap_error",
      ),
    agentLogText:
      "easydeploymesh-agent: bootstrap file is missing server or enrollmentToken",
  });

  assert.equal(result.verdict, "blocked");
  assert.equal(result.code, "bootstrap_invalid");
  assert.equal(diagnosticExitCode(result), 1);
});

test("classifies registration retries caused by an unreachable control plane", () => {
  const result = analyzeWinPeRuntime({
    reportText: baseReport.replace(
      "EASYDEPLOYMESH_DIAG_V1|control.health|ok|status=200",
      "EASYDEPLOYMESH_DIAG_V1|control.health|connect_error",
    ),
    agentLogText:
      "Agent registration failed (recovery attempt 1): error sending request for url (http://192.168.10.1:7760/api/v1/agents/register): tcp connect error: No connection could be made because the target machine actively refused it; retrying registration in 1 second(s), bounded at 30 second(s)",
  });

  assert.equal(result.verdict, "blocked");
  assert.equal(result.code, "control_plane_unreachable");
  assert.equal(result.registration, "retrying");
});

test("a current healthy report probe wins over a historical registration transport error", () => {
  const result = analyzeWinPeRuntime({
    reportText: baseReport,
    agentLogText:
      "Agent registration failed (recovery attempt 1): tcp connect error: connection refused",
  });

  assert.equal(result.verdict, "inconclusive");
  assert.equal(result.code, "control_plane_healthy_agent_not_registered");
});

test("a current absent process marker wins over historical accepted lifecycle events", () => {
  const result = analyzeWinPeRuntime({
    reportText: baseReport.replace(
      "EASYDEPLOYMESH_DIAG_V1|agent.process|running",
      "EASYDEPLOYMESH_DIAG_V1|agent.process|absent",
    ),
    agentLogText: [
      "Registered device 00000000-0000-0000-0000-000000000001 with EasyDeployMesh at http://192.168.10.1:7760",
      "Heartbeat accepted at 2026-08-16T03:28:45Z",
    ].join("\n"),
  });

  assert.equal(result.verdict, "blocked");
  assert.equal(result.code, "agent_process_not_running");
  assert.equal(result.registration, "accepted");
});

test("an unknown current process state remains inconclusive", () => {
  const result = analyzeWinPeRuntime({
    reportText: baseReport.replace(
      "EASYDEPLOYMESH_DIAG_V1|agent.process|running",
      "EASYDEPLOYMESH_DIAG_V1|agent.process|unknown",
    ),
  });

  assert.equal(result.verdict, "inconclusive");
  assert.equal(result.code, "control_plane_healthy_agent_not_registered");
});

test("uses the read-only health probe when no Agent log was created", () => {
  const result = analyzeWinPeRuntime({
    reportText: baseReport.replace(
      "EASYDEPLOYMESH_DIAG_V1|control.health|ok|status=200",
      "EASYDEPLOYMESH_DIAG_V1|control.health|connect_error",
    ),
    agentLogText: "",
  });

  assert.equal(result.verdict, "blocked");
  assert.equal(result.code, "control_plane_unreachable");
  assert.equal(result.registration, "not_observed");
});

test("a failed health probe with unusable local networking is classified locally", () => {
  const result = analyzeWinPeRuntime({
    reportText: baseReport
      .replace(
        "EASYDEPLOYMESH_DIAG_V1|control.health|ok|status=200",
        "EASYDEPLOYMESH_DIAG_V1|control.health|connect_error",
      )
      .replace(
        "==== Routes and neighbors ====",
        [
          "EASYDEPLOYMESH_DIAG_V1|network.ipv4|usable",
          "EASYDEPLOYMESH_DIAG_V1|network.route_to_control|absent",
          "==== Routes and neighbors ====",
        ].join("\n"),
      ),
  });

  assert.equal(result.verdict, "blocked");
  assert.equal(result.code, "network_unavailable");
});

test("a failed health probe with usable addressing and a route reaches the control-plane classification", () => {
  const result = analyzeWinPeRuntime({
    reportText: baseReport
      .replace(
        "EASYDEPLOYMESH_DIAG_V1|control.health|ok|status=200",
        "EASYDEPLOYMESH_DIAG_V1|control.health|connect_error",
      )
      .replace(
        "==== Routes and neighbors ====",
        [
          "EASYDEPLOYMESH_DIAG_V1|network.ipv4|usable",
          "EASYDEPLOYMESH_DIAG_V1|network.route_to_control|present",
          "==== Routes and neighbors ====",
        ].join("\n"),
      ),
  });

  assert.equal(result.code, "control_plane_unreachable");
});

test("unknown network markers do not overclaim the cause of a failed health probe", () => {
  const result = analyzeWinPeRuntime({
    reportText: baseReport
      .replace(
        "EASYDEPLOYMESH_DIAG_V1|control.health|ok|status=200",
        "EASYDEPLOYMESH_DIAG_V1|control.health|connect_error",
      )
      .replace(
        "==== Routes and neighbors ====",
        [
          "EASYDEPLOYMESH_DIAG_V1|network.ipv4|unknown",
          "EASYDEPLOYMESH_DIAG_V1|network.route_to_control|unknown",
          "==== Routes and neighbors ====",
        ].join("\n"),
      ),
  });

  assert.equal(result.code, "control_plane_unreachable");
  assert.match(result.summary, /could not distinguish/i);
});

test("distinguishes a healthy control endpoint from an unobserved Agent startup", () => {
  const result = analyzeWinPeRuntime({
    reportText: baseReport,
    agentLogText: "",
  });

  assert.equal(result.verdict, "inconclusive");
  assert.equal(result.code, "control_plane_healthy_agent_not_registered");
});

test("reports successful registration separately from a zero-disk deployment blocker", () => {
  const result = analyzeWinPeRuntime({
    reportText: baseReport,
    agentLogText: [
      "Registered device 00000000-0000-0000-0000-000000000001 with EasyDeployMesh at http://192.168.10.1:7760",
      "Heartbeat accepted at 2026-08-16T03:28:45Z",
      "Disk inventory probe failed: no physical disks; continuing registration and heartbeats with an empty disk list",
      "Skipping deployment job claim because the latest heartbeat reported no disks",
    ].join("\n"),
  });

  assert.equal(result.verdict, "blocked");
  assert.equal(result.code, "registered_no_disks");
  assert.equal(result.registration, "accepted");
  assert.equal(result.heartbeat, "accepted");
  assert.equal(result.deployment, "not_claimed");
});

test("a later claimed job wins over an earlier no-disk decision", () => {
  const result = analyzeWinPeRuntime({
    reportText: baseReport,
    agentLogText: [
      "Registered device 00000000-0000-0000-0000-000000000001 with EasyDeployMesh at http://192.168.10.1:7760",
      "Heartbeat accepted at 2026-08-16T03:28:45Z",
      "Skipping deployment job claim because the latest heartbeat reported no disks",
      "Claimed deployment job 00000000-0000-0000-0000-000000000002",
    ].join("\n"),
  });

  assert.equal(result.verdict, "partial");
  assert.equal(result.code, "job_claimed");
  assert.equal(result.deployment, "claimed");
});

test("a later no-disk decision wins over an earlier claimed job", () => {
  const result = analyzeWinPeRuntime({
    reportText: baseReport,
    agentLogText: [
      "Registered device 00000000-0000-0000-0000-000000000001 with EasyDeployMesh at http://192.168.10.1:7760",
      "Heartbeat accepted at 2026-08-16T03:28:45Z",
      "Claimed deployment job 00000000-0000-0000-0000-000000000002",
      "Skipping deployment job claim because the latest heartbeat reported no disks",
    ].join("\n"),
  });

  assert.equal(result.verdict, "blocked");
  assert.equal(result.code, "registered_no_disks");
});

test("identifies the current GHO executor rejection after a job was claimed", () => {
  const result = analyzeWinPeRuntime({
    reportText: baseReport,
    agentLogText: [
      "Registered device 00000000-0000-0000-0000-000000000001 with EasyDeployMesh at http://192.168.10.1:7760",
      "Heartbeat accepted at 2026-08-16T03:28:45Z",
      "Claimed deployment job 00000000-0000-0000-0000-000000000002",
      "Deployment job 00000000-0000-0000-0000-000000000002 failed and was reported to EasyDeployMesh: the first automated executor supports WIM and ESD deployment only; continuing agent loop",
    ].join("\n"),
  });

  assert.equal(result.verdict, "blocked");
  assert.equal(result.code, "unsupported_deployment_executor");
  assert.equal(result.registration, "accepted");
  assert.equal(result.deployment, "failed");
});

test("returns a distinct partial result when the Agent is healthy but no job was observed", () => {
  const result = analyzeWinPeRuntime({
    reportText: baseReport,
    agentLogText: [
      "Registered device 00000000-0000-0000-0000-000000000001 with EasyDeployMesh at http://192.168.10.1:7760",
      "Heartbeat accepted at 2026-08-16T03:28:45Z",
    ].join("\n"),
  });

  assert.equal(result.verdict, "partial");
  assert.equal(result.code, "registered_waiting_for_job");
  assert.equal(result.registration, "accepted");
  assert.equal(result.heartbeat, "accepted");
  assert.equal(diagnosticExitCode(result), 2);
});

test("a later accepted heartbeat wins over earlier recovery retries", () => {
  const result = analyzeWinPeRuntime({
    reportText: baseReport,
    agentLogText: [
      "Agent registration failed (recovery attempt 1): temporary transport failure; retrying registration in 1 second(s), bounded at 30 second(s)",
      "Registered device 00000000-0000-0000-0000-000000000001 with EasyDeployMesh at http://192.168.10.1:7760",
      "Heartbeat accepted at 2026-08-16T03:28:45Z",
    ].join("\n"),
  });

  assert.equal(result.code, "registered_waiting_for_job");
  assert.equal(result.registration, "accepted");
  assert.equal(result.heartbeat, "accepted");
});

test("an accepted heartbeat wins over an invalid decoy bootstrap in the report", () => {
  const result = analyzeWinPeRuntime({
    reportText: baseReport
      .replace(
        "EASYDEPLOYMESH_DIAG_V1|control.health|ok|status=200",
        "EASYDEPLOYMESH_DIAG_V1|control.health|not_run|reason=missing_runtime_input",
      )
      .replace(
        "EASYDEPLOYMESH_DIAG_V1|control.health|not_run|reason=missing_runtime_input",
        "File: X:\\Boot\\easydeploymesh-bootstrap.json\nServer: [unreadable JSON]\nEnrollment token: [MISSING]\nEASYDEPLOYMESH_DIAG_V1|control.health|not_run|reason=missing_runtime_input",
      ),
    agentLogText: [
      "Registered device 00000000-0000-0000-0000-000000000001 with EasyDeployMesh at http://192.168.10.1:7760",
      "Heartbeat accepted at 2026-08-16T03:28:45Z",
    ].join("\n"),
  });

  assert.equal(result.code, "registered_waiting_for_job");
  assert.equal(result.registration, "accepted");
});

test("a valid decoy bootstrap cannot mask an invalid authoritative bootstrap", () => {
  const result = analyzeWinPeRuntime({
    reportText: baseReport
      .replace("Server: http://192.168.10.1:7760", "Server: [unreadable JSON]")
      .replace("Enrollment token: [PRESENT, REDACTED]", "Enrollment token: [MISSING]")
      .replace(
        "EASYDEPLOYMESH_DIAG_V1|control.health|ok|status=200",
        [
          "File: X:\\Boot\\easydeploymesh-bootstrap.json",
          "Server: http://192.168.10.1:7760",
          "Enrollment token: [PRESENT, REDACTED]",
          "EASYDEPLOYMESH_DIAG_V1|control.health|not_run|reason=missing_runtime_input",
        ].join("\n"),
      ),
  });

  assert.equal(result.verdict, "blocked");
  assert.equal(result.code, "bootstrap_invalid");
});

test("never copies enrollment or authorization material into its result", () => {
  const secret = "easydeploymesh_enroll_example_secret_that_must_not_escape";
  const result = analyzeWinPeRuntime({
    reportText: `${baseReport}\n${secret}\nAuthorization: Bearer secret`,
    agentLogText: `Agent registration failed: ${secret}`,
  });

  const serialized = JSON.stringify(result);
  assert.doesNotMatch(serialized, /easydeploymesh_enroll_/i);
  assert.doesNotMatch(serialized, /authorization/i);
  assert.doesNotMatch(serialized, /bearer/i);
});

test("analysis results expose only the stable public schema", () => {
  const analysis = analyzeWinPeRuntime({
    reportText: baseReport,
    agentLogText: [
      "Agent registration failed (recovery attempt 1): temporary failure",
      "Claimed deployment job 00000000-0000-0000-0000-000000000002",
    ].join("\n"),
  });

  assert.deepEqual(Object.keys(analysis).sort(), [
    "code",
    "deployment",
    "heartbeat",
    "nextAction",
    "registration",
    "schemaVersion",
    "summary",
    "verdict",
  ]);
  assert.doesNotMatch(JSON.stringify(analysis), /acceptedEver|lastHeartbeatLine|lastNoDisksLine|unsupported|00000000/i);
});

test("deployment passes only with a complete report and the real Agent completion marker", () => {
  const completedLog = [
    "Registered device 00000000-0000-0000-0000-000000000001 with EasyDeployMesh at http://192.168.10.1:7760",
    "Heartbeat accepted at 2026-08-16T03:28:45Z",
    "Claimed deployment job 00000000-0000-0000-0000-000000000002",
    "EASYDEPLOYMESH_DIAG_V1|job.completion|reported_success",
  ].join("\n");
  const complete = analyzeWinPeRuntime({ reportText: baseReport, agentLogText: completedLog });
  const truncated = analyzeWinPeRuntime({
    reportText: baseReport.replace("EASYDEPLOYMESH_DIAG_V1|collector.complete|ok", ""),
    agentLogText: completedLog,
  });

  assert.equal(complete.verdict, "pass");
  assert.equal(complete.code, "deployment_completed");
  assert.equal(truncated.verdict, "inconclusive");
  assert.equal(truncated.code, "report_incomplete");
});

test("CLI reads only the fixed diagnostic files and returns a sanitized JSON verdict", async () => {
  const directory = await mkdtemp(join(tmpdir(), "easydeploymesh-runtime-analysis-"));
  try {
    const secret = "easydeploymesh_enroll_cli_secret";
    await Promise.all([
      writeFile(
        join(directory, "winpe-runtime.txt"),
        baseReport.replace(
          "EASYDEPLOYMESH_DIAG_V1|collector.complete|ok",
          `${secret}\nEASYDEPLOYMESH_DIAG_V1|collector.complete|ok`,
        ),
      ),
      writeFile(
        join(directory, "easydeploymesh-agent.sanitized.log"),
        [
          `Agent registration failed (recovery attempt 1): ${secret}; retrying registration in 1 second(s), bounded at 30 second(s)`,
          "Registered device 00000000-0000-0000-0000-000000000001 with EasyDeployMesh at http://192.168.10.1:7760",
          "Heartbeat accepted at 2026-08-16T03:28:45Z",
        ].join("\n"),
      ),
    ]);

    const command = runCli("--json", directory);
    assert.equal(command.status, 2, `${command.stdout}\n${command.stderr}`);
    assert.equal(command.stderr, "");
    assert.equal(JSON.parse(command.stdout).code, "registered_waiting_for_job");
    assert.doesNotMatch(command.stdout, /easydeploymesh_enroll_/i);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("CLI returns zero only for a complete report with a non-empty matching Agent log", async () => {
  const directory = await mkdtemp(join(tmpdir(), "easydeploymesh-runtime-complete-"));
  try {
    await Promise.all([
      writeFile(join(directory, "winpe-runtime.txt"), baseReport),
      writeFile(
        join(directory, "easydeploymesh-agent.sanitized.log"),
        [
          "Registered device 00000000-0000-0000-0000-000000000001 with EasyDeployMesh at http://192.168.10.1:7760",
          "Heartbeat accepted at 2026-08-16T03:28:45Z",
          "Claimed deployment job 00000000-0000-0000-0000-000000000002",
          "EASYDEPLOYMESH_DIAG_V1|job.completion|reported_success",
        ].join("\n"),
      ),
    ]);

    const command = runCli("--json", directory);
    assert.equal(command.status, 0, `${command.stdout}\n${command.stderr}`);
    assert.equal(JSON.parse(command.stdout).code, "deployment_completed");
    assert.equal(command.stderr, "");
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("CLI maps a structurally valid blocker to exit one", async () => {
  const directory = await mkdtemp(join(tmpdir(), "easydeploymesh-runtime-blocked-"));
  try {
    await Promise.all([
      writeFile(
        join(directory, "winpe-runtime.txt"),
        baseReport.replace("EASYDEPLOYMESH_DIAG_V1|agent.binary|present", "EASYDEPLOYMESH_DIAG_V1|agent.binary|missing"),
      ),
      writeFile(join(directory, "easydeploymesh-agent.sanitized.log"), "sanitized log has no lifecycle event"),
    ]);

    const command = runCli("--json", directory);
    assert.equal(command.status, 1, `${command.stdout}\n${command.stderr}`);
    assert.equal(JSON.parse(command.stdout).code, "agent_binary_missing");
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("CLI rejects every mismatch between the report Agent-log marker and sibling file", async () => {
  const cases = [
    ["present", undefined],
    ["present", ""],
    ["empty", undefined],
    ["empty", "non-empty"],
    ["missing", ""],
    ["missing", "non-empty"],
  ];

  for (const [marker, logContents] of cases) {
    const directory = await mkdtemp(join(tmpdir(), "easydeploymesh-runtime-log-state-"));
    try {
      await writeFile(
        join(directory, "winpe-runtime.txt"),
        baseReport.replace("EASYDEPLOYMESH_DIAG_V1|agent.log|present", `EASYDEPLOYMESH_DIAG_V1|agent.log|${marker}`),
      );
      if (logContents !== undefined) {
        await writeFile(join(directory, "easydeploymesh-agent.sanitized.log"), logContents);
      }

      const command = runCli("--json", directory);
      assert.equal(command.status, 65, `${marker}/${String(logContents)}: ${command.stdout}\n${command.stderr}`);
      assert.equal(command.stdout, "");
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  }
});

test("CLI accepts matching empty and missing Agent-log states", async () => {
  for (const marker of ["empty", "missing"]) {
    const directory = await mkdtemp(join(tmpdir(), "easydeploymesh-runtime-log-valid-"));
    try {
      await writeFile(
        join(directory, "winpe-runtime.txt"),
        baseReport.replace("EASYDEPLOYMESH_DIAG_V1|agent.log|present", `EASYDEPLOYMESH_DIAG_V1|agent.log|${marker}`),
      );
      if (marker === "empty") {
        await writeFile(join(directory, "easydeploymesh-agent.sanitized.log"), "");
      }

      const command = runCli("--json", directory);
      assert.equal(command.status, 2, `${marker}: ${command.stdout}\n${command.stderr}`);
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  }
});

test("CLI rejects truncated and forged-success reports as data errors", async () => {
  const secret = "easydeploymesh_enroll_forged_success_secret";
  for (const reportText of [
    baseReport.replace("EASYDEPLOYMESH_DIAG_V1|collector.complete|ok", secret),
    baseReport.replace(
      "EASYDEPLOYMESH_DIAG_V1|collector.complete|ok",
      [
        "Registered device 00000000-0000-0000-0000-000000000001 with EasyDeployMesh at http://example.invalid",
        "Heartbeat accepted at 2026-08-16T03:28:45Z",
        "Claimed deployment job 00000000-0000-0000-0000-000000000002",
        "EASYDEPLOYMESH_DIAG_V1|job.completion|reported_success",
        "EASYDEPLOYMESH_DIAG_V1|collector.complete|ok",
      ].join("\n"),
    ).replace("EASYDEPLOYMESH_DIAG_V1|agent.log|present", "EASYDEPLOYMESH_DIAG_V1|agent.log|missing"),
  ]) {
    const directory = await mkdtemp(join(tmpdir(), "easydeploymesh-runtime-invalid-"));
    try {
      await writeFile(join(directory, "winpe-runtime.txt"), reportText);
      const command = runCli("--json", directory);
      assert.equal(command.status, 65, `${command.stdout}\n${command.stderr}`);
      assert.equal(command.stdout, "");
      assert.doesNotMatch(command.stderr, /easydeploymesh_enroll_|reported_success|Registered device/i);
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  }
});

test("CLI uses sysexits-style codes without echoing unknown options or missing paths", () => {
  const secret = "easydeploymesh_enroll_cli_path_secret";
  const unknown = runCli(`--unknown-${secret}`);
  const missing = runCli("--json", join(tmpdir(), secret, "winpe-runtime.txt"));

  assert.equal(unknown.status, 64, `${unknown.stdout}\n${unknown.stderr}`);
  assert.equal(missing.status, 74, `${missing.stdout}\n${missing.stderr}`);
  for (const command of [unknown, missing]) {
    assert.equal(command.stdout, "");
    assert.doesNotMatch(command.stderr, /easydeploymesh_enroll_/i);
  }
});
