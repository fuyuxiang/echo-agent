import { cleanNativeReviewEnvironment } from "./lib/native-review-isolation.mjs";
import { nativeReviewOutput } from './lib/native-review-output.mjs';
import { spawn, spawnSync } from "node:child_process";
import { createHash } from 'node:crypto';
import { mkdtempSync, existsSync, readFileSync, copyFileSync, mkdirSync, unlinkSync, realpathSync, readdirSync, chmodSync, writeFileSync, lstatSync } from "node:fs";
import { join, resolve, relative, isAbsolute } from "node:path";
import { tmpdir } from "node:os";
import { setTimeout as delay } from "node:timers/promises";
import { runNativeOfficeReview, verifyNativeOfficeReceipts } from './native-office-review.mjs';

const executable = process.argv[2];
if (!executable || !existsSync(executable)) throw new Error("Pass a built desktop executable");
const outputRoot = nativeReviewOutput('native-desktop', 'UI_REVIEW_OUTPUT');
mkdirSync(outputRoot, { recursive: true });
const output = mkdtempSync(join(outputRoot, "run-"));
const home = realpathSync(mkdtempSync(join(tmpdir(), "echo-native-review-")));
// Prevent an isolated acceptance launch importing a retired real data home.
const legacy = join(home, "empty-legacy");
mkdirSync(legacy);
const seedConfig = process.env.NATIVE_REVIEW_SEED_CONFIG;
const seedAudio = process.env.NATIVE_REVIEW_AUDIO;
const mediaRequested = !!(seedConfig || seedAudio);
const extendedRequested = process.env.NATIVE_REVIEW_EXTENDED === '1';
if (mediaRequested) {
  if (!seedConfig || !seedAudio) throw new Error('Native media acceptance requires both config and audio resources');
  copyFileSync(seedConfig, join(home, 'config.toml'));
  chmodSync(join(home, 'config.toml'), 0o600);
  const workspace = join(home, 'workspaces', 'default');
  mkdirSync(workspace, { recursive: true, mode: 0o700 });
  copyFileSync(seedAudio, join(workspace, '中文会议验收.wav'));
  chmodSync(join(workspace, '中文会议验收.wav'), 0o600);
}
console.log(`Isolated native review home: ${home}`);
console.log(`Current native evidence directory: ${output}`);
const failures = [];
let firstReport;
let activeApp;
let interrupted = false;
const stopOwnedApp = () => {
  interrupted = true;
  activeApp?.kill('SIGTERM');
};
process.once('SIGINT', stopOwnedApp);
process.once('SIGTERM', stopOwnedApp);
const launchEnv = { ...cleanNativeReviewEnvironment(process.env), GROK_HOME: legacy, ECHO_DESKTOP_REVIEW: "1" };
launchEnv.ECHO_DESKTOP_REVIEW_MEDIA = mediaRequested ? '1' : '0';
launchEnv.ECHO_DESKTOP_REVIEW_EXTENDED = extendedRequested ? '1' : '0';
// Exercise the production login/relaunch path, where a parent environment is
// unavailable and the selected profile is carried by --data-home.
delete launchEnv.ECHO_AGENT_HOME;
for (const phase of ["first-launch", "restart"]) {
  if (interrupted) { failures.push('Native acceptance interrupted'); break; }
  const reportPath = join(home, "native-review.json");
  if (existsSync(reportPath)) unlinkSync(reportPath);
  let launchError;
  const startedAt = Date.now();
  const app = spawn(resolve(executable), ["--data-home", home], {
    env: { ...launchEnv, ECHO_DESKTOP_REVIEW_PHASE: phase },
    stdio: "inherit",
  });
  activeApp = app;
  const exit = new Promise((resolve) => { app.once("exit", resolve); app.once("error", (error) => { launchError = error; resolve(); }); });
  let processEnded = false;
  void exit.then(() => { processEnded = true; });
  const officeJob = phase === 'first-launch' && mediaRequested ? (async () => {
    const workspace = join(home, 'workspaces', 'default');
    const ready = join(workspace, 'native-office-ready.json');
    while (!processEnded && !existsSync(ready)) await delay(150);
    if (processEnded) return;
    let result;
    try {
      const request = JSON.parse(readFileSync(ready, 'utf8'));
      result = await runNativeOfficeReview({ home, request, output });
    } catch (error) { result = { passed: false, error: String(error) }; }
    // Only test artifacts are written here. Production document creation goes
    // through Office MCP and the application's authoritative session binding.
    writeFileSync(join(workspace, 'native-office-done.json'), JSON.stringify(result), { mode: 0o600 });
  })() : Promise.resolve();
  try {
    const phaseTimeout = extendedRequested ? (phase === 'first-launch' ? 900000 : 600000)
      : mediaRequested ? 420000 : 180000;
    const finished = await Promise.race([exit.then(() => true), delay(phaseTimeout, false, { ref: false })]);
    if (launchError) throw launchError;
    if (!finished) throw new Error(`${phase}: native application did not complete within ${phaseTimeout / 1000} seconds`);
    await officeJob;
    if (!existsSync(reportPath)) throw new Error(`${phase}: native report missing; retained ${home}`);
    const report = JSON.parse(readFileSync(reportPath, "utf8"));
    copyFileSync(reportPath, join(output, `${phase}.json`));
    console.log(JSON.stringify({ phase, exitCode: app.exitCode, report }));
    if (!Array.isArray(report.errors) || !Array.isArray(report.checks)) throw new Error(`${phase}: invalid native report schema`);
    if (report.phase !== phase) throw new Error(`${phase}: report belongs to ${String(report.phase)}`);
    const finishedAt = Date.parse(report.finishedAt);
    if (!Number.isFinite(finishedAt) || finishedAt < startedAt || finishedAt > Date.now() + 1000) throw new Error(`${phase}: native report is not from this launch`);
    if (phase === "first-launch") firstReport = report;
    const required = ["native-shell", "native-webview-state", "settings-entry", "runtime-model-ready", "automation-capabilities",
      ...(phase === "first-launch"
        ? ["empty-session-catalog", "native-initial-model-probe", "native-model-selection", "composer-native-send", "real-model-response", "native-stop-task", "native-stop-recovery", "native-rename-persistence", "native-project-create", "native-project-association", "native-workspace-file", "native-workspace-boundary"]
        : ["restart-session-load", "native-restart-task", "native-project-restore", "native-workspace-file"]),
      ...(mediaRequested ? (phase === 'first-launch'
        ? ['native-agent-office-task', 'native-markdown-render', 'native-meeting-import', 'native-meeting-process', 'native-meeting-edit', 'native-office-files']
        : ['native-meeting-restore', 'native-office-restore']) : []),
      ...(extendedRequested ? ['native-plan-approval', 'native-knowledge-memory', 'native-scheduled-task'] : []),
    ];
    const missing = required.filter((id) => report.checks.filter((check) => check.id === id).length !== 1);
    const incomplete = report.checks.filter((check) => check.result !== "passed");
    const evidenceFor = (id) => report.checks.find((check) => check.id === id)?.evidence;
    if (extendedRequested) {
      const knowledge = report.knowledge;
      const automation = report.automation;
      const plan = report.plan;
      if (report.nativeContext?.extendedRequested !== true || !knowledge?.passed || knowledge.errors?.length
        || !automation?.passed || automation.errors?.length
        || !plan?.passed || plan.errors?.length || !plan.checks?.length
        || plan.checks.some((check) => check.result !== 'passed')
        || knowledge.expected?.home !== home || automation.expected?.home !== home || plan.expected?.home !== home) {
        throw new Error(`${phase}: missing actual extended native plan/knowledge/automation evidence`);
      }
      const planState = plan.expected;
      if (!/^[A-Za-z0-9-]+$/.test(planState.sessionId || '')
        || planState.cwd !== join(home, 'workspaces', 'default')
        || typeof planState.artifactPath !== 'string'
        || !planState.artifactPath.startsWith(`${planState.cwd}/原生计划验收-`)
        || !planState.artifactPath.endsWith('/批准后成果.txt')
        || !lstatSync(planState.artifactPath).isFile() || lstatSync(planState.artifactPath).isSymbolicLink()
        || !realpathSync(planState.artifactPath).startsWith(`${planState.cwd}/`)
        || typeof planState.revisedApproval?.planContent !== 'string'
        || !planState.revisedApproval.planContent.includes(planState.revisedMarker)) {
        throw new Error(`${phase}: native plan approval has no revised on-disk content`);
      }
      const planFiles = readdirSync(join(home, 'sessions'), { withFileTypes: true })
        .filter((entry) => entry.isDirectory())
        .map((entry) => join(home, 'sessions', entry.name, planState.sessionId, 'plan.md'))
        .filter((path) => existsSync(path));
      if (planFiles.length !== 1 || !lstatSync(planFiles[0]).isFile() || lstatSync(planFiles[0]).isSymbolicLink()
        || lstatSync(resolve(planFiles[0], '..')).isSymbolicLink()) {
        throw new Error(`${phase}: actual plan metadata is missing, ambiguous or symbolic`);
      }
      const savedPlan = readFileSync(planFiles[0], 'utf8');
      const savedPlanHash = createHash('sha256').update(savedPlan).digest('hex');
      if (!savedPlan.includes(planState.revisedMarker) || !savedPlan.includes(planState.artifactPath)
        || readFileSync(planState.artifactPath, 'utf8') !== planState.content) {
        throw new Error(`${phase}: real saved plan/TXT did not retain the approved revision`);
      }
      if (phase === 'restart') {
        const firstPlan = JSON.parse(readFileSync(join(output, 'first-launch-plan-filesystem.json'), 'utf8'));
        if (savedPlanHash !== firstPlan.planSha256) throw new Error('restart: actual finished plan metadata changed');
      }
      writeFileSync(join(output, `${phase}-plan-filesystem.json`), JSON.stringify({
        planPath: planFiles[0], planSha256: savedPlanHash,
        approvedPlanSha256: createHash('sha256').update(planState.revisedApproval.planContent).digest('hex'),
        identicalToApprovedPlan: savedPlan === planState.revisedApproval.planContent,
        revisedContentRetained: true, artifactPath: planState.artifactPath,
        artifactSha256: createHash('sha256').update(planState.content).digest('hex'),
      }, null, 2));
      const memory = readFileSync(join(home, 'memory', 'MEMORY.md'), 'utf8');
      if (memory !== knowledge.expected.memory.content
        || createHash('sha256').update(memory).digest('hex') !== knowledge.expected.memory.revision) {
        throw new Error(`${phase}: native memory content/revision differs from actual disk`);
      }
      const sources = JSON.parse(readFileSync(join(home, 'local-knowledge-sources.json'), 'utf8'));
      if (!Array.isArray(sources) || sources.length !== 1
        || sources[0].id !== knowledge.expected.activeSource.id
        || sources[0].root !== knowledge.expected.activeSource.root) {
        throw new Error(`${phase}: native knowledge revocation differs from actual disk`);
      }
      const records = JSON.parse(readFileSync(join(home, 'echoagent-automation-records.json'), 'utf8'));
      const runs = Array.isArray(records) ? records : records.records;
      const run = Array.isArray(runs) && runs.find((item) => item.id === automation.expected.recordId);
      if (!run || run.status !== 'success' || run.sessionId !== automation.expected.sessionId
        || run.cwd !== automation.expected.cwd || run.modelId !== automation.expected.modelId) {
        throw new Error(`${phase}: actual saved scheduled execution is not successful or lost its binding`);
      }
    }
    const artifact = evidenceFor("native-workspace-file");
    if (artifact) {
      const inside = relative(home, artifact.path || "");
      if (inside === ".." || inside.startsWith(`..${process.platform === "win32" ? "\\" : "/"}`) || isAbsolute(inside)) throw new Error(`${phase}: artifact is outside isolated home`);
      const expectedContent = "EchoAgent 原生文件验收\n中文、空格路径与重启读回。\n";
      if (artifact.content !== expectedContent || readFileSync(artifact.path, "utf8") !== expectedContent) throw new Error(`${phase}: saved artifact content differs from native readback`);
    }
    if (phase === "first-launch") {
      const sent = evidenceFor("composer-native-send");
      const model = evidenceFor("real-model-response");
      if (report.checks.some((check) => check.id === "composer-native-send" && check.result === "passed")
        && (!report.sessionId || !report.workspace || sent?.sessionId !== report.sessionId || sent?.cwd !== report.workspace || sent?.userMessageVisible !== true)) {
        throw new Error(`${phase}: missing actual native session evidence`);
      }
      if (report.checks.some((check) => check.id === "real-model-response" && check.result === "passed")
        && (model?.status !== "completed" || typeof model?.answer !== "string" || !model.answer.includes("验收成功"))) {
        throw new Error(`${phase}: missing successful real model response`);
      }
      const stopped = evidenceFor('native-stop-task');
      const recovered = evidenceFor('native-stop-recovery');
      if (!stopped || stopped.sessionId !== report.sessionId || stopped.status !== 'stopped' || stopped.clicked !== true
        || !recovered || recovered.sessionId !== report.sessionId || recovered.cwd !== report.workspace
        || recovered.status !== 'completed' || recovered.newUserMessageVisible !== true || !recovered.answer?.includes('验收成功')) {
        throw new Error(`${phase}: actual native stop and next-turn recovery evidence is missing`);
      }
      if (report.sessionId) {
        const sessionsRoot = join(home, "sessions");
        const summaries = existsSync(sessionsRoot) ? readdirSync(sessionsRoot, { withFileTypes: true })
          .filter((entry) => entry.isDirectory())
          .map((entry) => join(sessionsRoot, entry.name, report.sessionId, "summary.json"))
          .filter((path) => existsSync(path)) : [];
        if (summaries.length !== 1) throw new Error(`${phase}: missing unique persisted session summary`);
        const summary = JSON.parse(readFileSync(summaries[0], "utf8"));
        const selected = report.selectedModelId;
        if (!selected || (summary.current_model_id !== selected && summary.current_model_id !== selected.split("/").at(-1))) {
          throw new Error(`${phase}: actual saved model does not match UI selection`);
        }
      }
    }
    const projectExpectation = phase === "first-launch" ? report.project : firstReport?.project;
    const sessionExpectation = phase === "first-launch" ? report.sessionId : firstReport?.sessionId;
    if (projectExpectation && sessionExpectation) {
      const projects = JSON.parse(readFileSync(join(home, "echoagent-projects.json"), "utf8"));
      const project = Array.isArray(projects) && projects.find((item) => item.id === projectExpectation.id);
      if (!project || project.name !== projectExpectation.name || project.defaultModelId !== "echoagent-ojlab/Echo Reed"
        || !project.conversations?.some((item) => item.sessionId === sessionExpectation)) {
        throw new Error(`${phase}: actual saved project or session binding differs from UI evidence`);
      }
    }
    if (mediaRequested) {
      const agentTask = firstReport?.agentOffice;
      if (!agentTask || agentTask.status !== 'completed' || agentTask.toolVisible !== true
        || agentTask.receiptVisibleInDetails !== true || typeof agentTask.receiptPath !== 'string'
        || agentTask.userMessageVisible !== true || agentTask.sessionId !== firstReport.sessionId
        || agentTask.cwd !== firstReport.workspace || !agentTask.answer?.includes('验收成功')) {
        throw new Error(`${phase}: missing actual native Agent Office task evidence`);
      }
      const directory = join(firstReport.workspace, 'EchoAgent成果');
      const agentFiles = readdirSync(directory).filter((name) => name.startsWith('原生Agent办公验收-') && name.endsWith('.docx'));
      if (agentFiles.length !== 1) throw new Error(`${phase}: native Agent did not save one unique requested Word file`);
      const path = realpathSync(join(directory, agentFiles[0]));
      if (relative(directory, path) !== agentFiles[0]) throw new Error(`${phase}: native Agent document is outside its workspace`);
      if (agentTask.receiptPath !== path) throw new Error(`${phase}: actual Word file differs from the visible Office receipt`);
      const opened = spawnSync(process.env.NATIVE_REVIEW_PYTHON || 'python3', ['-c',
        'import sys; from docx import Document; d=Document(sys.argv[1]); s="\\n".join(p.text for p in d.paragraphs); assert "原生Agent办公验收" in s and "今天确认任务状态，明天完成复核。" in s; print("python-docx content verified")', path],
      { encoding: 'utf8', timeout: 15000 });
      if (opened.error || opened.status !== 0) throw new Error(`${phase}: saved native Agent Word content could not be verified`);
      const data = readFileSync(path);
      const agentReceipt = { path, byteSize: data.length, sha256: createHash('sha256').update(data).digest('hex'), openedWith: 'python-docx', phase };
      if (phase === 'first-launch') firstReport.agentOffice.receipt = agentReceipt;
      else if (agentReceipt.path !== firstReport.agentOffice.receipt?.path || agentReceipt.sha256 !== firstReport.agentOffice.receipt?.sha256) {
        throw new Error(`${phase}: native Agent Word document changed during restart`);
      }
      writeFileSync(join(output, `native-agent-office-${phase}.json`), JSON.stringify(agentReceipt, null, 2));
      const office = report.office;
      if (!office?.passed || !Array.isArray(office.receipts)) throw new Error(`${phase}: missing actual native Office evidence`);
      await verifyNativeOfficeReceipts({ home,
        request: { cwd: firstReport?.workspace || report.workspace, sessionId: firstReport?.sessionId || report.sessionId, phase: 'first-launch' },
        receipts: office.receipts });
      if (phase === 'restart' && JSON.stringify(office.receipts) !== JSON.stringify(firstReport?.office?.receipts)) {
        throw new Error(`${phase}: Office files differ from the first launch`);
      }
      const meeting = phase === 'first-launch' ? report.meeting : evidenceFor('native-meeting-restore');
      if (!meeting?.id || !meeting.audioPath) throw new Error(`${phase}: missing actual meeting evidence`);
      const inside = relative(home, meeting.audioPath);
      if (inside === '..' || inside.startsWith(`..${process.platform === 'win32' ? '\\' : '/'}`) || isAbsolute(inside)) {
        throw new Error(`${phase}: meeting audio is outside isolated home`);
      }
      const audio = readFileSync(meeting.audioPath);
      if (audio.length < 100000 || audio.subarray(0, 4).toString() !== 'RIFF' || audio.subarray(8, 12).toString() !== 'WAVE') {
        throw new Error(`${phase}: actual meeting audio is missing or is not a WAV recording`);
      }
      if (typeof meeting.minutes !== 'string' || !meeting.minutes.includes('验收人工编辑：明天完成复核。')) {
        throw new Error(`${phase}: edited meeting minutes were not restored`);
      }
      if (phase === 'restart' && (meeting.id !== firstReport?.meeting?.id || meeting.audioPath !== firstReport?.meeting?.audioPath
        || meeting.minutes !== firstReport?.meeting?.minutes)) throw new Error(`${phase}: meeting readback differs from the first launch`);
    }
    if (phase === "restart" && firstReport?.sessionId) {
      const restored = report.checks.find((check) => check.id === "restart-session-load");
      const evidence = restored?.evidence;
      if (evidence?.sessionId !== firstReport.sessionId || evidence?.cwd !== firstReport.workspace
        || evidence?.title !== "原生验收保留会话" || evidence?.userMessageVisible !== true
        || evidence?.selectedInUi !== true
        || (firstReport.checks.some((check) => check.id === "real-model-response" && check.result === "passed") && evidence?.assistantSuccessVisible !== true)) {
        throw new Error(`${phase}: restored UI does not match the first launch session`);
      }
      const project = evidenceFor("native-project-restore");
      if (!project || project.id !== firstReport.project?.id || project.name !== firstReport.project?.name
        || project.sessionId !== firstReport.sessionId || project.cwd !== firstReport.workspace || project.visibleInSidebar !== true) {
        throw new Error(`${phase}: restored project does not match the first launch`);
      }
      const resumed = evidenceFor('native-restart-task');
      if (!resumed || resumed.sessionId !== firstReport.sessionId || resumed.cwd !== firstReport.workspace
        || resumed.status !== 'completed' || resumed.newUserMessageVisible !== true
        || !resumed.modelLabel?.includes('Echo Reed') || !resumed.answer?.includes('验收成功')) {
        throw new Error(`${phase}: restored session did not execute a successful new native task`);
      }
    }
    if (app.exitCode !== 0 || report.errors.length || missing.length || incomplete.length) {
      throw new Error(`${phase}: native review incomplete (exit=${app.exitCode}; missing=${missing.join(",")}; checks=${incomplete.map((check) => `${check.id}:${check.result}`).join(",")}; errors=${report.errors.join("; ")})`);
    }
  } catch (error) {
    // Model availability must not turn into a pass. Retain the failure and
    // still exercise the restart so local persistence can be inspected.
    failures.push(String(error));
    console.error(String(error));
  } finally {
    if (app.exitCode === null) { app.kill(); await Promise.race([exit, delay(3000, undefined, { ref: false })]); if (app.exitCode === null) app.kill("SIGKILL"); }
    activeApp = undefined;
  }
}
console.log(`Native evidence and isolated data retained: ${output}; ${home}`);
if (failures.length) {
  console.error(`Native acceptance failed: ${failures.join("\n")}`);
  process.exitCode = 1;
}
