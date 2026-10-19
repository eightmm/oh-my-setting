import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

const helper = fileURLToPath(new URL("./hook_state.py", import.meta.url));
const maxOutput = 16 * 1024;

function hook(command, payload) {
  return new Promise((resolve) => {
    let child;
    let timer;
    let output = Buffer.alloc(0);
    let settled = false;
    const finish = (ok = false, notice = "") => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve(ok ? output.toString("utf8").replace(/\r/g, "").trim() : notice);
    };
    try {
      const env = { ...process.env };
      // Stdin is authoritative; an inherited hook payload may belong to a parent.
      delete env.OMS_HOOK_PAYLOAD;
      // Pi panel enrollment belongs to Codex; Claude-shaped fields alone default
      // to Claude in hook_state and would withhold this main's room mail.
      env.OMS_HOOK_AGENT = "codex";
      child = spawn("python3", [helper, command], {
        env, stdio: ["pipe", "pipe", "ignore"], windowsHide: true,
      });
      child.on("error", () => finish());
      child.on("close", (code) => finish(code === 0));
      child.stdin.on("error", () => finish());
      child.stdout.on("data", (chunk) => {
        if (output.length + chunk.length > maxOutput) {
          // Mail may already have advanced its delivery cursor, but remains
          // unacknowledged. Make explicit polling visible instead of partial JSON.
          child.kill();
          finish(false, "[oms room] Hook context exceeded the output limit. Read oms room updates before relying on peer state; delivery is not acknowledgment.");
        } else {
          output = Buffer.concat([output, chunk]);
        }
      });
      timer = setTimeout(() => {
        child.kill();
        finish(false, "[oms room] Hook context timed out. Read oms room updates before relying on peer state; delivery is not acknowledgment.");
      }, 10000);
      child.stdin.end(JSON.stringify(payload));
    } catch {
      if (child) child.kill();
      finish();
    }
  });
}

export default function (pi) {
  const enabled = () => Boolean(process.env.OMS_PANEL_MAIN_ATTEMPT)
    && process.env.OMS_HARNESS_CHILD !== "1";
  if (!enabled()) return;

  // One identity per main run, also across an extension reload: the room
  // binds mail delivery to the first session id it sees.
  const session = `oms-pi-${process.env.OMS_PANEL_MAIN_ATTEMPT}`;
  const payload = (event, ctx, fields = {}) => ({
    hook_event_name: event, cwd: ctx.cwd, session_id: session, ...fields,
  });
  let pending = Promise.resolve("");
  let toolPending = false;
  const enqueue = (command, data) => {
    pending = pending.then(() => hook(command, data)).catch(() => "");
    return pending;
  };

  const resetContext = async (source, ctx) => {
    if (!enabled()) return;
    await enqueue("relay-hint", payload("SessionStart", ctx, { source }));
  };
  pi.on("session_start", async (event, ctx) => {
    // Reload keeps the conversation; new/resumed/forked sessions replace it.
    if (event.reason === "reload") return;
    const source = event.reason === "new" ? "clear"
      : ["resume", "fork"].includes(event.reason) ? "resume" : "startup";
    await resetContext(source, ctx);
  });
  pi.on("session_compact", async (_event, ctx) => {
    await resetContext("compact", ctx);
  });
  pi.on("before_agent_start", async (event, ctx) => {
    if (!enabled()) return;
    const content = await enqueue("panel-hook", payload("UserPromptSubmit", ctx, { prompt: event.prompt }));
    if (content) return { message: { customType: "oms-room", content, display: false } };
  });
  pi.on("tool_execution_end", (event, ctx) => {
    if (!enabled() || toolPending) return;
    toolPending = true;
    // Coalesce in-flight tool heartbeats; Stop waits behind them to stay idle.
    void enqueue("panel-hook", payload("PostToolUse", ctx, { tool_name: event.toolName }))
      .finally(() => { toolPending = false; });
  });
  // agent_end closes one low-level run; retries, recovery and queued work can
  // follow, so Stop waits for agent_settled (Pi 0.80.4+).
  pi.on("agent_settled", async (_event, ctx) => {
    if (!enabled()) return;
    await enqueue("relay", payload("Stop", ctx));
  });
}
