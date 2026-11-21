#!/usr/bin/env node
// Phase 8 UI gate (agent B): the native Slint UI driven through Slint's embedded MCP introspection server,
// backed by the real agent hub (harness-agent-hub) with scripted fake ACP agents. Spec:
// Worse/docs/specs/harness/phase-8-ui.md §7.
//
//   node Worse/scripts/e2e/phase8-ui.mjs
//   PHASE8_UI_SKIP_BUILD=1       reuse the last build in Worse/.work/tmp/phase8-ui-bin
//   PHASE8_UI_IDLE_S=<s>         long idle sample point (default 300 = 5 min)
//   PHASE8_UI_COMPARE_S=<s>      idle period per renderer in the comparison (default 60)
//   PHASE8_UI_SKIP_RAM=1         scenarios only (development; the gate result then does not cover RAM)
//
// Prints PHASE8_UI_OK or PHASE8_UI_FAIL <step> <reason> as the last line. Writes
// Worse/.work/phase8/phase8-ui.json and PNG screenshots next to it.
//
// Safety: installIsolatedHome() runs first; HARNESS_HOME and the workspace live inside it. The only agents are
// harness-fake-agent processes (configured through HARNESS_UI_AGENTS); no real agent CLI, provider or remote server
// is involved. Only process trees rooted at PIDs this gate spawned are inspected or stopped.
import { installIsolatedHome } from "./lib/isolatedHome.mjs";

const { home } = installIsolatedHome("phase8-ui");

const { spawn, spawnSync } = await import("node:child_process");
const { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } = await import("node:fs");
const net = await import("node:net");
const os = (await import("node:os")).default;
const path = (await import("node:path")).default;
const process = (await import("node:process")).default;
const { fileURLToPath } = await import("node:url");
const { SlintMcp, assertNoRowOverlap, byLabel, byLabelPrefix, label, sleep } = await import("./phase8-ui/mcp.mjs");
const { meanLuminance, sideBySide } = await import("./phase8-ui/png.mjs");
const { SCENES, captureScenes } = await import("./phase8-ui/scenes.mjs");
const { killTrees, processTree, sampleTree, stillAlive, summarizeSample } = await import("./phase7-perf/procs.mjs");

const worse = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const reportDir = path.join(worse, ".work/phase8");
const binDir = path.join(worse, ".work/tmp/phase8-ui-bin");
const exeName = (name) => (process.platform === "win32" ? `${name}.exe` : name);
const IDLE_S = Number(process.env.PHASE8_UI_IDLE_S ?? 300);
const COMPARE_S = Number(process.env.PHASE8_UI_COMPARE_S ?? 60);
const RAM_LIMIT_MB = 50;

const harnessHome = path.join(home, ".harness");
const workspace = path.join(home, "workspace");
const fakeState = path.join(home, "fake-agent-state");
const roots = new Set();
const results = { startedAt: new Date().toISOString(), steps: [], ram: {}, screenshots: [] };
let currentMcp = null;

class StepError extends Error {
  constructor(step, message) {
    super(message);
    this.step = step;
  }
}

function log(line) {
  process.stdout.write(`[phase8-ui] ${line}\n`);
}

async function step(name, fn) {
  const started = Date.now();
  try {
    const value = await fn();
    results.steps.push({ name, ok: true, ms: Date.now() - started });
    log(`ok   ${name} (${Date.now() - started} ms)`);
    return value;
  } catch (error) {
    results.steps.push({ name, ok: false, ms: Date.now() - started, error: String(error?.message ?? error) });
    // 失败现场：记录当前可见元素标签和截图，便于定位。
    if (currentMcp) {
      results.failureLabels = await currentMcp
        .elements()
        .then((list) => list.map(label).filter(Boolean).slice(0, 200))
        .catch((e) => [String(e?.message ?? e)]);
      await shot(currentMcp, `fail-${name.replace(/[^a-z0-9]+/gi, "-")}`).catch(() => {});
    }
    throw error instanceof StepError ? error : new StepError(name, error?.message ?? String(error));
  }
}

// ---- build -------------------------------------------------------------------------------------------------

function cargo(args) {
  // 隔离 home 改写了 USERPROFILE；cargo/rustup 需要真实账户下的工具链目录（只构建，不运行 Harness）。
  const accountHome = os.userInfo().homedir;
  const env = {
    ...process.env,
    CARGO_HOME: process.env.CARGO_HOME ?? path.join(accountHome, ".cargo"),
    RUSTUP_HOME: process.env.RUSTUP_HOME ?? path.join(accountHome, ".rustup"),
  };
  const result = spawnSync("cargo", args, { cwd: worse, env, stdio: ["ignore", "inherit", "inherit"], windowsHide: true });
  if (result.status !== 0) throw new Error(`cargo ${args.join(" ")} exited ${String(result.status)}`);
}

function build() {
  mkdirSync(binDir, { recursive: true });
  // 修复：cargo 遵循 CARGO_TARGET_DIR，而这里写死了 target/release，会把旧的（甚至非 automation 的）二进制拷进 bin 目录，
  // 导致 "MCP server not ready"。与 phase9 门禁一致，从实际的目标目录取产物。
  const release = path.join(process.env.CARGO_TARGET_DIR ? path.resolve(process.env.CARGO_TARGET_DIR) : path.join(worse, "target"), "release");
  const copy = (name, as = name) => copyFileSync(path.join(release, exeName(name)), path.join(binDir, exeName(as)));
  if (process.env.PHASE8_UI_SKIP_BUILD !== "1") {
    cargo(["build", "--release", "-p", "harness-fake-agent", "-p", "harness-agent-hub", "--bins"]);
    copy("harness-fake-agent");
    copy("harness-memory-mcp");
    // 同一输出路径：先构建带自动化通道的版本并复制，再构建产品版本（RAM 只测产品版本）。
    cargo(["build", "--release", "-p", "harness-slint", "--features", "automation"]);
    copy("harness-slint", "harness-slint-automation");
    cargo(["build", "--release", "-p", "harness-slint"]);
    copy("harness-slint");
  }
  for (const name of ["harness-fake-agent", "harness-memory-mcp", "harness-slint", "harness-slint-automation"]) {
    if (!existsSync(path.join(binDir, exeName(name)))) throw new Error(`${name} missing in ${binDir}`);
  }
}

// ---- fixtures + launch -------------------------------------------------------------------------------------

function fixtures() {
  mkdirSync(path.join(workspace, "src"), { recursive: true });
  writeFileSync(path.join(workspace, "README.md"), "# demo workspace\n");
  writeFileSync(path.join(workspace, "src", "main.rs"), "fn main() {}\n");
  mkdirSync(fakeState, { recursive: true });
  const agent = (id, name) => ({
    id,
    name,
    program: path.join(binDir, exeName("harness-fake-agent")),
    args: ["--name", id],
    env: { FAKE_AGENT_STATE: fakeState, FAKE_AGENT_LOG: path.join(fakeState, "log.jsonl") },
  });
  const agents = [
    agent("claude-code", "Claude Code"),
    agent("codex", "Codex"),
    // agy: configured but not installed (availability probe only; nothing is spawned).
    { id: "agy", name: "agy", program: path.join(home, "not-installed", exeName("agy")), args: [] },
  ];
  const file = path.join(home, "ui-agents.json");
  writeFileSync(file, JSON.stringify(agents, null, 2));
  return file;
}

function freePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      server.close(() => resolve(port));
    });
  });
}

async function launch({ automation, agentsFile, backend, fakeCore = false }) {
  const exe = path.join(binDir, exeName(automation ? "harness-slint-automation" : "harness-slint"));
  const env = {
    ...process.env,
    HARNESS_HOME: harnessHome,
    HARNESS_WORKSPACE: workspace,
    HARNESS_UI_AGENTS: agentsFile,
    HARNESS_MEMORY_MCP: path.join(binDir, exeName("harness-memory-mcp")),
  };
  delete env.SLINT_MCP_PORT;
  delete env.SLINT_BACKEND;
  delete env.HARNESS_UI_CORE;
  if (fakeCore) {
    // 内置 fake core：不启动任何 agent 进程，单独的 HARNESS_HOME，不影响真实 hub 场景的持久化数据。
    env.HARNESS_UI_CORE = "fake";
    env.HARNESS_HOME = path.join(home, ".harness-fake");
  }
  let port = null;
  if (automation) {
    port = await freePort();
    env.SLINT_MCP_PORT = String(port);
  }
  if (backend) env.SLINT_BACKEND = backend;
  const child = spawn(exe, [], { cwd: workspace, env, stdio: ["ignore", "ignore", "pipe"], windowsHide: false });
  const stderr = [];
  child.stderr.setEncoding("utf8").on("data", (chunk) => stderr.push(chunk));
  roots.add(child.pid);
  const exited = new Promise((resolve) => child.once("exit", (code) => resolve(code)));
  const app = { child, pid: child.pid, exited, stderr, mcp: null };
  if (automation) {
    app.mcp = new SlintMcp(port);
    await app.mcp.connect();
    currentMcp = app.mcp;
  }
  return app;
}

/** WM_CLOSE first (graceful teardown: hub shutdown stops agents), force only as a fallback. */
async function close(app) {
  const tree = await processTree([app.pid]).catch(() => []);
  spawnSync("taskkill", ["/PID", String(app.pid)], { stdio: "ignore", windowsHide: true });
  const graceful = await Promise.race([app.exited.then(() => true), sleep(15_000).then(() => false)]);
  if (!graceful) await killTrees([app.pid]);
  await sleep(500);
  const alive = await stillAlive(tree);
  if (alive.length > 0) {
    await killTrees(alive.map((p) => p.pid));
    throw new Error(`processes left after close: ${alive.map((p) => `${p.name}#${p.pid}`).join(", ")}`);
  }
  roots.delete(app.pid);
  if (currentMcp === app.mcp) currentMcp = null;
  return { graceful, treeSize: tree.length };
}

// ---- UI helpers --------------------------------------------------------------------------------------------

async function shot(mcp, name) {
  const png = await mcp.screenshot();
  const file = path.join(reportDir, `ui-${name}.png`);
  writeFileSync(file, png);
  results.screenshots.push(path.relative(worse, file).replaceAll("\\", "/"));
  return png;
}

async function button(mcp, text) {
  return mcp.waitFor(`button "${text}"`, (e) => label(e) === text && e.accessibleRole === "Button");
}

async function press(mcp, text) {
  await mcp.activate(await button(mcp, text));
}

/** Opens a Select (combobox) by its label and picks the option with that label (Phase 9.2 Language dropdown). */
async function choose(mcp, comboLabel, option) {
  await mcp.activate(await mcp.waitFor(`combobox "${comboLabel}"`, (e) => label(e) === comboLabel && e.accessibleRole === "Combobox"));
  await press(mcp, option);
}

async function typeAndSend(mcp, text) {
  const input = await mcp.waitFor("message input", (e) => label(e) === "Message" || label(e) === "消息");
  await mcp.click(input);
  await mcp.setValue(input, text);
  await mcp.key("\n");
}

async function pickAgent(mcp, name) {
  const picker = await mcp.waitFor("agent picker", byLabelPrefix("Agent: "));
  await mcp.activate(picker);
  const option = await mcp.waitFor(`agent option ${name}`, byLabel(`agent-option ${name}`));
  if (!option.accessibleEnabled) throw new Error(`agent ${name} is disabled in the picker`);
  await mcp.activate(option);
  await mcp.waitFor(`picker shows ${name}`, byLabel(`Agent: ${name}`));
}

const reply = (agent, includes) => byLabelPrefix(`${agent}: `, includes);
// 会话列表项与文件树项同为 ListItem；文件树只含 fixtures() 写入的名字。
const FIXTURE_NAMES = new Set(["README.md", "src", "main.rs"]);
const isSessionItem = (e) => e.accessibleRole === "ListItem" && !FIXTURE_NAMES.has(label(e));

// ---- scenarios ---------------------------------------------------------------------------------------------

async function scenarios(agentsFile) {
  const app = await step("start", () => launch({ automation: true, agentsFile }));
  const { mcp } = app;
  try {
    await step("start: shell visible", async () => {
      await button(mcp, "New session");
      await press(mcp, "Files");
      await mcp.waitFor("file tree README.md", byLabel("README.md"));
      await mcp.waitFor("picker shows sticky Claude Code", byLabel("Agent: Claude Code"));
    });

    await step("new session", async () => {
      await press(mcp, "New session");
      await mcp.waitFor("session list item", isSessionItem);
    });

    await step("keyboard: Shift+Enter inserts a newline", async () => {
      const input = await mcp.waitFor("message input", byLabel("Message"));
      await mcp.click(input);
      await mcp.setValue(input, "line one");
      await mcp.key("\u0010", "Press");
      await mcp.key("\n");
      await mcp.key("\u0010", "Release");
      const after = await mcp.find(byLabel("Message"));
      if (!after?.accessibleValue?.includes("\n")) throw new Error(`draft has no newline: ${JSON.stringify(after?.accessibleValue)}`);
      if (await mcp.find(byLabel("line one"))) throw new Error("Shift+Enter sent the message");
    });

    await step("send + streamed reply (Claude Code)", async () => {
      await typeAndSend(mcp, "hello one [[slow 20]]");
      await mcp.waitFor("user bubble", byLabel("hello one [[slow 20]]"));
      // 流式中间态：已有部分输出且尚未出现最终摘要。
      const partial = await mcp.waitFor("partial streamed text", (e) => reply("Claude Code", "chunk-")(e) && !label(e).includes("reply to:"));
      await mcp.waitFor("final reply", reply("Claude Code", "reply to: hello one"));
      results.streamPartial = label(partial).slice(0, 80);
      await shot(mcp, "light-chat");
    });

    await step("two agents in one session", async () => {
      await pickAgent(mcp, "Codex");
      await typeAndSend(mcp, "second message");
      await mcp.waitFor("Codex reply", reply("Codex", "reply to: second message"));
      await mcp.waitFor("Claude Code reply still shown", reply("Claude Code", "reply to: hello one"));
      await sleep(400);
      // 长回复（换行 4 行）后紧跟一条用户消息：8.2 遗留的行高不足会让下一条消息压在回复上。
      results.chatRowsChecked = await assertNoRowOverlap(mcp);
    });

    await step("stop (Esc)", async () => {
      await typeAndSend(mcp, "stop me [[slow 200]]");
      await mcp.waitFor("streaming started", (e) => reply("Codex", "chunk-")(e) && !label(e).includes("reply to:"));
      // 8.2：运行中发送的消息进入队列（输入框上方可取消），取消后不得再运行。
      await typeAndSend(mcp, "queued then cancelled");
      await mcp.waitFor("queued row", byLabel("Queued: queued then cancelled"));
      if (await mcp.find(byLabel("queued then cancelled"))) throw new Error("queued message already shown as a user bubble");
      await shot(mcp, "queue");
      await press(mcp, "Cancel queued message: queued then cancelled");
      await mcp.waitGone("queued row", byLabel("Queued: queued then cancelled"));
      await button(mcp, "Stop");
      await mcp.key("\u001b");
      await mcp.waitFor("Stopped status", byLabel("Stopped"));
      await mcp.waitGone("Stop button", (e) => label(e) === "Stop" && e.accessibleRole === "Button");
      await sleep(1500);
      if (await mcp.find(byLabel("queued then cancelled"))) throw new Error("cancelled queued message ran");
    });

    await step("permission approve", async () => {
      await typeAndSend(mcp, "write please [[permission]]");
      await mcp.waitFor("tool awaiting permission", byLabel("write demo.txt · Awaiting permission"));
      await shot(mcp, "permission");
      // 9.2 起权限卡片列出 agent 提供的选项（fake ACP agent: "Allow once" / "Deny"），不再是固定的 Approve 按钮。
      await press(mcp, "Allow once");
      await mcp.waitFor("approved reply", reply("Codex", "permission=allowed"));
      await mcp.waitFor("tool done", byLabel("write demo.txt · Done"));
    });

    await step("permission deny", async () => {
      await typeAndSend(mcp, "write again [[permission]]");
      await mcp.waitFor("tool awaiting permission", byLabel("write demo.txt · Awaiting permission"));
      await press(mcp, "Deny");
      await mcp.waitFor("denied reply", reply("Codex", "permission=denied"));
      await mcp.waitFor("tool denied", byLabel("write demo.txt · Denied"));
    });

    await step("failover on quota mid-stream", async () => {
      await pickAgent(mcp, "Claude Code");
      await typeAndSend(mcp, "fail over [[quota-mid claude-code]]");
      const notice = await mcp.waitFor("failover notice", byLabelPrefix("Switched from Claude Code to Codex: limit reached (resets "));
      await mcp.waitFor("continuation by Codex", reply("Codex", "reply to: fail over"));
      await mcp.waitFor("interrupted partial from Claude Code", reply("Claude Code", "partial-"));
      await mcp.waitFor("picker now shows Codex", byLabel("Agent: Codex"));
      results.failoverNotice = label(notice);
      // 8.2：重置时间按本地时间显示（HH:MM 或 MM-DD HH:MM），不再带 UTC。
      if (!/\(resets (\d\d-\d\d )?\d\d:\d\d\)$/.test(label(notice)) || label(notice).includes("UTC")) {
        throw new Error(`reset time is not a local HH:MM label: ${label(notice)}`);
      }
      await shot(mcp, "failover");
      await mcp.activate(await mcp.waitFor("agent picker", byLabel("Agent: Codex")));
      const exhausted = await mcp.waitFor("exhausted option", byLabel("agent-option Claude Code"));
      if (exhausted.accessibleEnabled) throw new Error("exhausted Claude Code is still enabled in the picker");
      if (!String(exhausted.accessibleDescription ?? "").startsWith("Limit reached")) {
        throw new Error(`exhausted description: ${exhausted.accessibleDescription}`);
      }
      const missing = await mcp.waitFor("not installed option", byLabel("agent-option agy"));
      if (missing.accessibleEnabled) throw new Error("not-installed agy is enabled");
      await mcp.key("\u001b");
      await mcp.waitGone("picker", byLabel("agent-option Codex"));
    });

    await step("all agents exhausted: paused, resumed by manual swap", async () => {
      await typeAndSend(mcp, "pause please [[quota-start codex]]");
      await mcp.waitFor("paused notice", byLabelPrefix("All agents reached their limits. Paused"));
      await mcp.waitFor("paused banner", byLabelPrefix("All agents reached their limits. New messages are queued"));
      await shot(mcp, "paused");
      // 手动切换会清除该 agent 的耗尽标记并恢复暂停的回合（spec phase-8-core §6.4）。
      await mcp.activate(await mcp.waitFor("agent picker", byLabel("Agent: Codex")));
      const option = await mcp.waitFor("exhausted Claude Code option", byLabel("agent-option Claude Code"));
      await mcp.activate(option);
      await mcp.waitFor("resumed by Claude Code", reply("Claude Code", "reply to: pause please"));
      await mcp.waitGone("paused banner", byLabelPrefix("All agents reached their limits. New messages are queued"));
    });

    await step("theme switch", async () => {
      // Phase 9.1：主题在“外观”页，语言在“通用”页。
      await press(mcp, "Settings");
      await press(mcp, "Appearance");
      await press(mcp, "Dark");
      await sleep(400);
      const dark = meanLuminance(await shot(mcp, "dark-settings"));
      await press(mcp, "Light");
      await sleep(400);
      const light = meanLuminance(await shot(mcp, "light-settings"));
      results.luminance = { dark: Math.round(dark), light: Math.round(light) };
      if (!(light - dark > 60)) throw new Error(`theme did not change the window (light ${light.toFixed(0)}, dark ${dark.toFixed(0)})`);
    });

    await step("language switch", async () => {
      await press(mcp, "General");
      // Phase 9.2：语言改为下拉选择（与 ZCode 一致），先打开下拉框再选项。
      await choose(mcp, "Language", "简体中文");
      await mcp.waitFor("zh-CN label", byLabel("新建会话"));
      await mcp.waitFor("zh-CN settings title", byLabel("设置"));
      await shot(mcp, "zh-cn");
      await choose(mcp, "语言", "English");
      await mcp.waitFor("en-US label", byLabel("New session"));
      // 重启后验证持久化：留在深色主题。
      await press(mcp, "Appearance");
      await press(mcp, "Dark");
      await mcp.key("\u001b");
      await mcp.waitGone("settings dialog", byLabel("Close settings"));
    });
  } finally {
    results.firstRun = await close(app);
  }

  const again = await step("restart", () => launch({ automation: true, agentsFile }));
  try {
    await step("restart restores sessions + settings", async () => {
      const item = await again.mcp.waitFor("restored session", isSessionItem);
      await again.mcp.activate(item);
      await again.mcp.waitFor("restored latest turn", reply("Claude Code", "reply to: pause please"));
      await again.mcp.waitFor("restored failover notice", byLabelPrefix("Switched from Claude Code to Codex"));
      // ListView 只实例化可见行：滚到顶部再检查最早的回合。
      await again.mcp.scrollAt(550, 300, 20_000);
      await again.mcp.waitFor("restored transcript", reply("Claude Code", "reply to: hello one"));
      await again.mcp.waitFor("restored Codex turn", reply("Codex", "reply to: second message"));
      const lum = meanLuminance(await shot(again.mcp, "restart-dark"));
      if (lum > 90) throw new Error(`dark theme not restored (luminance ${lum.toFixed(0)})`);
    });
  } finally {
    results.secondRun = await close(again);
  }
}
// Markdown rendering + ZCode comparison screenshots (fake core, same scenes as compare.mjs).
async function comparison() {
  const app = await step("compare: start (fake core)", () => launch({ automation: true, fakeCore: true }));
  try {
    await step("compare: markdown table, highlighted code, scenes", async () => {
      const refDir = path.join(reportDir, "ref");
      mkdirSync(refDir, { recursive: true });
      const checks = await captureScenes(app.mcp, async (name) => writeFileSync(path.join(refDir, `harness-${name}.png`), await app.mcp.screenshot()));
      results.markdown = checks;
      if (checks.table !== "table 3x3") throw new Error(`table not rendered as 3x3: ${checks.table}`);
      if (!checks.codeHighlighted) throw new Error("rust code block was not syntax highlighted");
      results.compare = [];
      for (const scene of SCENES) {
        const zcode = path.join(refDir, `zcode-${scene}.png`);
        if (!existsSync(zcode)) continue;
        const out = path.join(reportDir, `compare-${scene}.png`);
        writeFileSync(out, sideBySide(readFileSync(zcode), readFileSync(path.join(refDir, `harness-${scene}.png`))));
        results.compare.push(path.relative(worse, out).replaceAll("\\", "/"));
      }
    });
  } finally {
    results.compareRun = await close(app);
  }
}

// ---- RAM ---------------------------------------------------------------------------------------------------

const mb = (bytes) => +(bytes / 1048576).toFixed(1);

async function measure(agentsFile, backend, points) {
  const app = await launch({ automation: false, agentsFile, backend });
  const samples = {};
  try {
    const started = Date.now();
    for (const seconds of points) {
      await sleep(Math.max(0, started + seconds * 1000 - Date.now()));
      if (app.child.exitCode !== null) throw new Error(`app exited early: ${app.stderr.join("").slice(0, 400)}`);
      const summary = summarizeSample(await sampleTree([app.pid]));
      samples[`${seconds}s`] = {
        processes: summary.processCount,
        privateWorkingSetMb: mb(summary.privateWorkingSetBytes),
        workingSetMb: mb(summary.workingSetBytes),
        privateBytesMb: mb(summary.privateBytes),
        // 8.2：记录进程名，空闲时出现子进程（如 git/shell）可直接定位来源。
        byName: Object.fromEntries(Object.entries(summary.byName).map(([name, v]) => [name, { count: v.count, privateWorkingSetMb: mb(v.privateWorkingSet) }])),
      };
      log(`ram ${backend ?? "default"} @${seconds}s: private WS ${samples[`${seconds}s`].privateWorkingSetMb} MB, ${summary.processCount} process(es)`);
    }
  } finally {
    await close(app);
  }
  return samples;
}

// ---- main --------------------------------------------------------------------------------------------------

async function main() {
  mkdirSync(reportDir, { recursive: true });
  await step("build", async () => build());
  const agentsFile = fixtures();
  await scenarios(agentsFile);
  await comparison();
  if (process.env.PHASE8_UI_SKIP_RAM === "1") return;
  await step("ram: renderer comparison", async () => {
    results.ram.software = await measure(agentsFile, "winit-software", [COMPARE_S]);
    results.ram.femtovg = await measure(agentsFile, "winit-femtovg", [COMPARE_S]);
  });
  await step("ram: default build idle", async () => {
    results.ram.default = await measure(agentsFile, null, [60, IDLE_S]);
    const worst = Math.max(...Object.values(results.ram.default).map((s) => s.privateWorkingSetMb));
    if (worst > RAM_LIMIT_MB) throw new Error(`idle private working set ${worst} MB > ${RAM_LIMIT_MB} MB`);
  });
}

let failure = null;
try {
  await main();
} catch (error) {
  failure = error;
} finally {
  if (roots.size > 0) await killTrees([...roots]).catch(() => {});
  results.finishedAt = new Date().toISOString();
  results.idleSeconds = IDLE_S;
  results.ok = !failure;
  if (failure) results.failure = { step: failure.step ?? "main", message: String(failure.message ?? failure) };
  mkdirSync(reportDir, { recursive: true });
  writeFileSync(path.join(reportDir, "phase8-ui.json"), `${JSON.stringify(results, null, 2)}\n`);
}
if (failure) {
  process.stdout.write(`PHASE8_UI_FAIL ${failure.step ?? "main"} ${String(failure.message ?? failure).replace(/\s+/g, " ")}\n`);
  process.exitCode = 1;
} else {
  process.stdout.write("PHASE8_UI_OK\n");
}
