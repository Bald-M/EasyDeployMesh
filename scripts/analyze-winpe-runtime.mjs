#!/usr/bin/env node

import { lstat, readFile } from "node:fs/promises";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const RESULT_SCHEMA_VERSION = 1;
const MAX_INPUT_BYTES = 16 * 1024 * 1024;

const result = (fields) => ({
  schemaVersion: RESULT_SCHEMA_VERSION,
  verdict: fields.verdict,
  code: fields.code,
  summary: fields.summary,
  nextAction: fields.nextAction,
  registration: fields.registration ?? "not_observed",
  heartbeat: fields.heartbeat ?? "not_observed",
  deployment: fields.deployment ?? "not_observed",
});

const includesAny = (value, patterns) => patterns.some((pattern) => pattern.test(value));

const REPORT_HEADER = "EasyDeployMesh WinPE runtime diagnostics";
const REPORT_SECTIONS = [
  "WinPE and process state",
  "Bootstrap configuration (token redacted)",
  "Agent log",
  "Startup chain (sensitive lines omitted)",
  "Network interfaces and DNS",
  "Routes and neighbors",
  "Physical disks and volumes (read-only queries)",
  "Ghost and EIX executable inventory",
];
export function validateWinPeReport(reportText) {
  const report = String(reportText ?? "").replace(/^\uFEFF/, "");
  const lines = report.split(/\r?\n/);
  const errors = [];
  if (report.includes("\0")) errors.push("report_contains_nul");
  if (lines[0] !== REPORT_HEADER) errors.push("report_header_missing");

  let previousSection = -1;
  const sectionIndices = new Map();
  for (const section of REPORT_SECTIONS) {
    const heading = `==== ${section} ====`;
    const indices = lines.flatMap((line, index) => (line === heading ? [index] : []));
    if (indices.length !== 1) {
      errors.push(indices.length === 0 ? `section_missing:${section}` : `section_duplicate:${section}`);
      continue;
    }
    sectionIndices.set(section, indices[0]);
    if (indices[0] <= previousSection) errors.push(`section_out_of_order:${section}`);
    previousSection = indices[0];
  }

  const headings = lines.filter((line) => /^==== .* ====$/.test(line));
  const expectedHeadings = new Set(REPORT_SECTIONS.map((section) => `==== ${section} ====`));
  if (headings.length !== REPORT_SECTIONS.length || headings.some((line) => !expectedHeadings.has(line))) {
    errors.push("section_set_invalid");
  }
  if (lines.some((line) => /^EASYDEPLOYMESH_DIAG_V1\|job\.completion\|/.test(line))) {
    errors.push("agent_lifecycle_marker_in_report");
  }

  const completionIndices = lines.flatMap((line, index) =>
    line === "EASYDEPLOYMESH_DIAG_V1|collector.complete|ok" ? [index] : [],
  );
  if (completionIndices.length !== 1) {
    errors.push(completionIndices.length === 0 ? "collector_incomplete" : "collector_marker_duplicate");
  } else {
    const ghostSection = sectionIndices.get("Ghost and EIX executable inventory");
    if (ghostSection === undefined || completionIndices[0] <= ghostSection) {
      errors.push("collector_marker_misplaced");
    }
    if (lines.slice(completionIndices[0] + 1).some((line) => line !== "")) {
      errors.push("collector_trailing_content");
    }
  }

  const authoritativeMarkers = lines.flatMap((line, index) =>
    /^EASYDEPLOYMESH_DIAG_V1\|bootstrap\.authoritative\|(?:present|missing)$/.test(line) ? [index] : [],
  );
  if (authoritativeMarkers.length !== 1) {
    errors.push("bootstrap_authoritative_marker_invalid");
  } else {
    const sectionStart = sectionIndices.get("Bootstrap configuration (token redacted)");
    const sectionEnd = sectionIndices.get("Agent log");
    if (
      sectionStart === undefined ||
      sectionEnd === undefined ||
      authoritativeMarkers[0] <= sectionStart ||
      authoritativeMarkers[0] >= sectionEnd
    ) {
      errors.push("bootstrap_authoritative_marker_misplaced");
    }
  }

  const logMarkers = lines.flatMap((line, index) => {
    const state = line.match(/^EASYDEPLOYMESH_DIAG_V1\|agent\.log\|(present|empty|missing)$/)?.[1];
    return state ? [{ index, state }] : [];
  });
  if (logMarkers.length !== 1) {
    errors.push("agent_log_marker_invalid");
  } else {
    const sectionStart = sectionIndices.get("Agent log");
    const sectionEnd = sectionIndices.get("Startup chain (sensitive lines omitted)");
    if (
      sectionStart === undefined ||
      sectionEnd === undefined ||
      logMarkers[0].index <= sectionStart ||
      logMarkers[0].index >= sectionEnd
    ) {
      errors.push("agent_log_marker_misplaced");
    }
  }

  return {
    valid: errors.length === 0,
    complete: completionIndices.length === 1,
    errors,
    agentLogState: logMarkers.length === 1 ? logMarkers[0].state : "unknown",
  };
}

function authoritativeBootstrapBlock(report) {
  const lines = report.split(/\r?\n/);
  const start = lines.findIndex(
    (line) => line.toLowerCase() === "file: x:\\easydeploymesh\\easydeploymesh-bootstrap.json",
  );
  if (start < 0) return "";
  const endOffset = lines
    .slice(start + 1)
    .findIndex((line) => line.startsWith("File: ") || line.startsWith("==== "));
  const end = endOffset < 0 ? lines.length : start + 1 + endOffset;
  return lines.slice(start, end).join("\n");
}

function parseAgentLifecycle(log) {
  const state = {
    registration: "not_observed",
    heartbeat: "not_observed",
    acceptedEver: false,
    lastHeartbeatLine: -1,
    lastNoDisksLine: -1,
    lastJobLine: -1,
    job: null,
  };

  for (const [lineNumber, line] of log.split(/\r?\n/).entries()) {
    if (/^Agent registration failed \(recovery attempt \d+\):/i.test(line)) {
      state.registration = "retrying";
      state.heartbeat = "not_observed";
      continue;
    }
    if (/^Registered device [0-9a-f-]+ with EasyDeployMesh at /i.test(line)) {
      state.registration = "accepted";
      state.heartbeat = "not_observed";
      state.acceptedEver = true;
      continue;
    }
    if (/^Agent heartbeat failed \(recovery attempt \d+\):/i.test(line)) {
      state.heartbeat = "retrying";
      continue;
    }
    if (/^Heartbeat accepted at /i.test(line)) {
      state.registration = "accepted";
      state.heartbeat = "accepted";
      state.acceptedEver = true;
      state.lastHeartbeatLine = lineNumber;
      continue;
    }
    if (/Skipping deployment job claim because the latest heartbeat reported no disks/i.test(line)) {
      state.lastNoDisksLine = lineNumber;
      continue;
    }
    const claim = line.match(/^Claimed deployment job ([0-9a-f-]+)/i);
    if (claim) {
      state.job = { id: claim[1].toLowerCase(), state: "claimed", unsupported: false };
      state.lastJobLine = lineNumber;
      continue;
    }
    const failed = line.match(
      /^Deployment job ([0-9a-f-]+) failed and was reported to EasyDeployMesh:(.*)$/i,
    );
    if (failed && state.job?.id === failed[1].toLowerCase()) {
      state.job.state = "failed";
      state.lastJobLine = lineNumber;
      state.job.unsupported = /the first automated executor supports WIM and ESD deployment only/i.test(
        failed[2],
      );
      continue;
    }
    if (line === "EASYDEPLOYMESH_DIAG_V1|job.completion|reported_success" && state.job) {
      state.job.state = "completed";
      state.lastJobLine = lineNumber;
      continue;
    }
    if (line === "EASYDEPLOYMESH_DIAG_V1|job.completion|reported_failure" && state.job) {
      state.job.state = "failed";
      state.lastJobLine = lineNumber;
      continue;
    }
  }

  return {
    ...state,
    noDisks:
      state.lastHeartbeatLine >= 0 &&
      state.lastNoDisksLine > state.lastHeartbeatLine &&
      state.lastNoDisksLine > state.lastJobLine,
  };
}

/**
 * Classifies a EasyDeployMesh WinPE runtime report without returning source text.
 * The function deliberately relies on EasyDeployMesh-owned ASCII markers and Agent
 * messages; localized ipconfig, DiskPart, and tasklist output is informational.
 */
export function analyzeWinPeRuntime({ reportText, agentLogText = "" }) {
  const report = String(reportText ?? "").replace(/^\uFEFF/, "");
  const log = String(agentLogText ?? "").replace(/^\uFEFF/, "");
  const reportValidation = validateWinPeReport(report);
  const lifecycle = parseAgentLifecycle(log);

  if (
    /EASYDEPLOYMESH_DIAG_V1\|agent\.binary\|missing/i.test(report) ||
    /X:\\EasyDeployMesh\\easydeploymesh-agent\.exe is missing\./i.test(report)
  ) {
    return result({
      verdict: "blocked",
      code: "agent_binary_missing",
      summary: "The EasyDeployMesh Agent binary is missing from the running WinPE image.",
      nextAction: "Re-import the PE package so the current Agent is injected into boot.wim.",
      ...lifecycle,
    });
  }

  if (/EASYDEPLOYMESH_DIAG_V1\|agent\.version_probe\|failed(?:\||$)/i.test(report)) {
    return result({
      verdict: "blocked",
      code: "agent_binary_unusable",
      summary: "WinPE found the Agent binary, but its read-only version probe failed.",
      nextAction: "Check the WinPE loader dependencies and use the current x64 Agent build.",
      ...lifecycle,
    });
  }

  const bootstrapMissing =
    /EASYDEPLOYMESH_DIAG_V1\|bootstrap\.authoritative\|missing/i.test(report) ||
    (!/EASYDEPLOYMESH_DIAG_V1\|bootstrap\.authoritative\|present/i.test(report) &&
      !authoritativeBootstrapBlock(report)) ||
    /easydeploymesh-agent: --server or --bootstrap is required/i.test(log);
  const bootstrapBlock = authoritativeBootstrapBlock(report);
  const healthProbeReachedConfiguration =
    /EASYDEPLOYMESH_DIAG_V1\|control\.health\|(?:ok|connect_error|timeout|request_error|http_error|invalid_response|unhealthy)/i.test(
      report,
    );
  const bootstrapInvalid =
    bootstrapMissing ||
    (!healthProbeReachedConfiguration &&
      (/Server: \[(?:invalid or unsafe URL|unreadable JSON|missing)\]/i.test(bootstrapBlock) ||
        /Enrollment token: \[(?:MISSING|UNKNOWN[^\]]*)\]/i.test(bootstrapBlock))) ||
    /EASYDEPLOYMESH_DIAG_V1\|control\.health\|not_run\|reason=(?:bootstrap_error|server_invalid)/i.test(
      report,
    ) ||
    (!healthProbeReachedConfiguration &&
      (/easydeploymesh-agent: .*bootstrap file is missing server or enrollmentToken/i.test(log) ||
        /easydeploymesh-agent: .*server must be a valid http/i.test(log)));
  if (bootstrapInvalid) {
    return result({
      verdict: "blocked",
      code: bootstrapMissing ? "bootstrap_missing" : "bootstrap_invalid",
      summary: bootstrapMissing
        ? "The authoritative EasyDeployMesh bootstrap file was not found."
        : "The EasyDeployMesh bootstrap file is unreadable or incomplete.",
      nextAction: "Recreate the boot package while the control service is running, then re-import the PE media.",
      ...lifecycle,
    });
  }

  const controlHealthOk = /EASYDEPLOYMESH_DIAG_V1\|control\.health\|ok\|status=2\d\d/i.test(report);
  if (
    /^EASYDEPLOYMESH_DIAG_V1\|control\.health\|(?:connect_error|timeout|request_error)\r?$/im.test(
      report,
    )
  ) {
    const ipv4State = report.match(
      /^EASYDEPLOYMESH_DIAG_V1\|network\.ipv4\|(usable|unusable|unknown)\r?$/im,
    )?.[1]?.toLowerCase() ?? "unknown";
    const routeState = report.match(
      /^EASYDEPLOYMESH_DIAG_V1\|network\.route_to_control\|(present|absent|unknown)\r?$/im,
    )?.[1]?.toLowerCase() ?? "unknown";
    const localNetworkUnavailable = ipv4State === "unusable" || routeState === "absent";
    const networkCauseUnknown = ipv4State === "unknown" && routeState === "unknown";
    return result({
      verdict: "blocked",
      code: localNetworkUnavailable ? "network_unavailable" : "control_plane_unreachable",
      summary: localNetworkUnavailable
        ? "The current report shows unusable local addressing or no route to the configured control service."
        : networkCauseUnknown
          ? "The read-only probe could not reach the configured control service, and the report could not distinguish local networking from the remote endpoint."
          : "The Agent's read-only probe could not reach the configured control service despite usable local network evidence.",
      nextAction: localNetworkUnavailable
        ? "Load the target NIC driver and correct the WinPE address, gateway, or route."
        : "Check the firewall and the configured control-service host and port.",
      registration: lifecycle.acceptedEver ? "accepted" : lifecycle.registration,
      heartbeat: lifecycle.heartbeat,
    });
  }
  if (/EASYDEPLOYMESH_DIAG_V1\|control\.health\|http_error\|status=\d+/i.test(report)) {
    return result({
      verdict: "blocked",
      code: "control_plane_health_http_error",
      summary: "The configured endpoint responded, but its health route returned an HTTP error.",
      nextAction: "Confirm the bootstrap points to the current EasyDeployMesh control service rather than a proxy or old endpoint.",
      registration: lifecycle.acceptedEver ? "accepted" : lifecycle.registration,
      heartbeat: lifecycle.heartbeat,
    });
  }
  if (/^EASYDEPLOYMESH_DIAG_V1\|control\.health\|(?:invalid_response|unhealthy)(?:\|status=\d+)?\r?$/im.test(report)) {
    return result({
      verdict: "blocked",
      code: "control_plane_health_invalid",
      summary: "The configured endpoint did not return a valid healthy EasyDeployMesh response.",
      nextAction: "Confirm the host and port belong to the current EasyDeployMesh control service.",
      registration: lifecycle.acceptedEver ? "accepted" : lifecycle.registration,
      heartbeat: lifecycle.heartbeat,
    });
  }

  if (/^EASYDEPLOYMESH_DIAG_V1\|agent\.process\|absent\r?$/im.test(report)) {
    return result({
      verdict: "blocked",
      code: "agent_process_not_running",
      summary: "The Agent binary is usable, but the current report shows that its process is not running.",
      nextAction: "Repair the WinPE startup hook or start the long-running Agent before collecting diagnostics again.",
      registration: lifecycle.acceptedEver ? "accepted" : lifecycle.registration,
      heartbeat: lifecycle.heartbeat,
    });
  }

  if (lifecycle.registration === "retrying" && !controlHealthOk) {
    const enrollmentRejected = includesAny(log, [
      /Agent registration failed[^\r\n]*(?:HTTP status client error \(401|401 Unauthorized|status code 401)/i,
    ]);
    if (enrollmentRejected) {
      return result({
        verdict: "blocked",
        code: "enrollment_rejected",
        summary: "The control service rejected Agent enrollment.",
        nextAction: "Restart the control service, re-inject its new bootstrap, and boot the target again.",
        ...lifecycle,
      });
    }

    const noUsableMac = /no usable MAC address was found/i.test(log);
    if (noUsableMac) {
      return result({
        verdict: "blocked",
        code: "network_adapter_unavailable",
        summary: "The Agent could not discover a usable network adapter address.",
        nextAction: "Load the target NIC driver in WinPE and confirm wpeinit completed.",
        ...lifecycle,
      });
    }

    const unreachable = includesAny(log, [
      /error sending request for url/i,
      /tcp connect error/i,
      /connection (?:refused|timed out)/i,
      /actively refused/i,
      /dns error/i,
      /failed to lookup address/i,
    ]);
    return result({
      verdict: "blocked",
      code: unreachable ? "control_plane_unreachable" : "registration_retrying",
      summary: unreachable
        ? "The Agent is retrying because it cannot reach the configured control service."
        : "The Agent is running, but registration has not been accepted.",
      nextAction: unreachable
        ? "Check the WinPE address, route, firewall, and the configured control-service host and port."
        : "Inspect the sanitized Agent log together with the control-service log.",
      ...lifecycle,
    });
  }

  if (lifecycle.heartbeat === "accepted") {
    if (lifecycle.job?.unsupported) {
      return result({
        verdict: "blocked",
        code: "unsupported_deployment_executor",
        summary: "The Agent registered and claimed a job, but the selected image format has no executor.",
        nextAction: "Use a verified WIM/ESD image or finish the qualified GHO executor before re-arming.",
        registration: "accepted",
        heartbeat: "accepted",
        deployment: "failed",
      });
    }
    if (lifecycle.noDisks) {
      return result({
        verdict: "blocked",
        code: "registered_no_disks",
        summary: "The Agent is online, but it reported no deployable physical disk.",
        nextAction: "Load the storage driver in WinPE and confirm DiskPart and the Agent see the same disk.",
        registration: "accepted",
        heartbeat: "accepted",
        deployment: "not_claimed",
      });
    }
    if (lifecycle.job?.state === "completed") {
      if (!reportValidation.valid) {
        return result({
          verdict: "inconclusive",
          code: "report_incomplete",
          summary: "A success marker was found, but the diagnostic report is incomplete or malformed.",
          nextAction: "Run the current collector again and copy its complete output directory.",
          registration: "accepted",
          heartbeat: "accepted",
          deployment: "claimed",
        });
      }
      return result({
        verdict: "pass",
        code: "deployment_completed",
        summary: "Registration, heartbeat, and deployment completion were observed.",
        nextAction: "No runtime blocker was detected.",
        registration: "accepted",
        heartbeat: "accepted",
        deployment: "completed",
      });
    }
    if (lifecycle.job?.state === "failed") {
      return result({
        verdict: "blocked",
        code: "deployment_failed",
        summary: "The Agent registered and claimed a job, but deployment failed.",
        nextAction: "Inspect the corresponding service job progress and sanitized Agent failure.",
        registration: "accepted",
        heartbeat: "accepted",
        deployment: "failed",
      });
    }
    if (lifecycle.job?.state === "claimed") {
      return result({
        verdict: "partial",
        code: "job_claimed",
        summary: "The Agent is online and claimed a deployment job; completion was not observed in this capture.",
        nextAction: "Collect another report after the job finishes or inspect service-side progress.",
        registration: "accepted",
        heartbeat: "accepted",
        deployment: "claimed",
      });
    }
    return result({
      verdict: "partial",
      code: "registered_waiting_for_job",
      summary: "The Agent registered and heartbeats are accepted, but no deployment job was observed.",
      nextAction: "Create and start a deployment job for this device, then collect diagnostics again.",
      registration: "accepted",
      heartbeat: "accepted",
      deployment: "not_claimed",
    });
  }

  if (lifecycle.registration === "accepted") {
    return result({
      verdict: lifecycle.heartbeat === "retrying" ? "blocked" : "partial",
      code:
        lifecycle.heartbeat === "retrying"
          ? "heartbeat_retrying"
          : "registered_heartbeat_not_observed",
      summary:
        lifecycle.heartbeat === "retrying"
          ? "Registration succeeded, but the control service has not accepted a heartbeat."
          : "Registration was observed, but this capture does not prove an accepted heartbeat.",
      nextAction:
        lifecycle.heartbeat === "retrying"
          ? "Check whether the control service restarted or the device credential became stale."
          : "Leave the Agent running through one heartbeat interval and collect diagnostics again.",
      registration: "accepted",
      heartbeat: lifecycle.heartbeat,
    });
  }

  const agentVersionOk =
    /EASYDEPLOYMESH_DIAG_V1\|agent\.version_probe\|ok(?:\||$)/i.test(report) ||
    /easydeploymesh-agent\s+\d+\.\d+\.\d+/i.test(report);
  if (agentVersionOk) {
    return result({
      verdict: "inconclusive",
      code: controlHealthOk
        ? "control_plane_healthy_agent_not_registered"
        : "agent_startup_not_observed",
      summary: controlHealthOk
        ? "The Agent can reach a healthy control service, but no registration lifecycle event was captured."
        : "The Agent binary is runnable, but no registration lifecycle event was captured.",
      nextAction: controlHealthOk
        ? "Confirm the startup hook launched the long-running Agent and inspect its sanitized log."
        : "Confirm the startup hook launched the Agent and collect the sanitized Agent log.",
      ...lifecycle,
    });
  }

  return result({
    verdict: "inconclusive",
    code: "insufficient_evidence",
    summary: "The report does not contain enough stable evidence to classify Agent registration.",
    nextAction: "Run the current collector from X:\\EasyDeployMesh and include its complete output directory.",
    ...lifecycle,
  });
}

export function diagnosticExitCode(analysis) {
  if (analysis?.verdict === "pass") return 0;
  if (analysis?.verdict === "blocked") return 1;
  return 2;
}

class DiagnosticInputError extends Error {
  constructor(exitCode, safeMessage) {
    super(safeMessage);
    this.exitCode = exitCode;
  }
}

const missingInputError = () =>
  new DiagnosticInputError(74, "required diagnostic input was not found");
const invalidInputError = () =>
  new DiagnosticInputError(65, "diagnostic input failed integrity validation");

async function readRegularFile(path, { optional = false } = {}) {
  let stat;
  try {
    stat = await lstat(path);
  } catch (error) {
    if (error?.code === "ENOENT") {
      if (optional) return { exists: false, size: 0, text: "" };
      throw missingInputError();
    }
    throw invalidInputError();
  }
  if (!stat.isFile() || stat.isSymbolicLink()) {
    throw invalidInputError();
  }
  if (stat.size > MAX_INPUT_BYTES) {
    throw invalidInputError();
  }
  try {
    return { exists: true, size: stat.size, text: await readFile(path, "utf8") };
  } catch (error) {
    if (error?.code === "ENOENT" && !optional) throw missingInputError();
    if (error?.code === "ENOENT") return { exists: false, size: 0, text: "" };
    throw invalidInputError();
  }
}

async function loadDiagnosticInput(inputPath) {
  const absolute = resolve(inputPath);
  let stat;
  try {
    stat = await lstat(absolute);
  } catch (error) {
    if (error?.code === "ENOENT") throw missingInputError();
    throw invalidInputError();
  }
  if (stat.isSymbolicLink()) throw invalidInputError();

  const root = stat.isDirectory() ? absolute : dirname(absolute);
  const reportPath = stat.isDirectory() ? join(root, "winpe-runtime.txt") : absolute;
  if (!stat.isDirectory() && basename(reportPath).toLowerCase() !== "winpe-runtime.txt") {
    throw invalidInputError();
  }
  const reportFile = await readRegularFile(reportPath);
  const validation = validateWinPeReport(reportFile.text);
  if (!validation.valid) throw invalidInputError();

  const agentLogFile = await readRegularFile(join(root, "easydeploymesh-agent.sanitized.log"), {
    optional: true,
  });
  const logStateMatches =
    (validation.agentLogState === "present" && agentLogFile.exists && agentLogFile.size > 0) ||
    (validation.agentLogState === "empty" && agentLogFile.exists && agentLogFile.size === 0) ||
    (validation.agentLogState === "missing" && !agentLogFile.exists);
  if (!logStateMatches) throw invalidInputError();

  return { reportText: reportFile.text, agentLogText: agentLogFile.text };
}

function printHuman(analysis) {
  process.stdout.write(
    [
      `EasyDeployMesh WinPE diagnosis: ${analysis.verdict.toUpperCase()}`,
      `Code: ${analysis.code}`,
      `Registration: ${analysis.registration}`,
      `Heartbeat: ${analysis.heartbeat}`,
      `Deployment: ${analysis.deployment}`,
      `Summary: ${analysis.summary}`,
      `Next action: ${analysis.nextAction}`,
      "",
    ].join("\n"),
  );
}

function printUsage() {
  process.stderr.write(
    "Usage: node scripts/analyze-winpe-runtime.mjs [--json] <diagnostics-directory|winpe-runtime.txt>\n",
  );
}

async function main(argv) {
  let json = false;
  const operands = [];
  for (const argument of argv) {
    if (argument === "--json") {
      json = true;
    } else if (argument.startsWith("-")) {
      printUsage();
      return 64;
    } else {
      operands.push(argument);
    }
  }
  if (operands.length !== 1) {
    printUsage();
    return 64;
  }
  try {
    const input = await loadDiagnosticInput(operands[0]);
    const analysis = analyzeWinPeRuntime(input);
    if (json) process.stdout.write(`${JSON.stringify(analysis, null, 2)}\n`);
    else printHuman(analysis);
    return diagnosticExitCode(analysis);
  } catch (error) {
    const exitCode = error instanceof DiagnosticInputError ? error.exitCode : 65;
    const message =
      error instanceof DiagnosticInputError
        ? error.message
        : "diagnostic analysis failed without exposing input data";
    process.stderr.write(`Unable to analyze WinPE diagnostics: ${message}.\n`);
    return exitCode;
  }
}

const invokedPath = process.argv[1] ? resolve(process.argv[1]) : "";
if (invokedPath === fileURLToPath(import.meta.url)) {
  process.exitCode = await main(process.argv.slice(2));
}
