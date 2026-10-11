// The only credential in Pi is the capability for this invocation's loopback gateway.
// It moves out of process.env on first load so Pi's tool subprocesses never inherit it;
// a reloaded extension reads the same values back from this process.
const route = (globalThis[Symbol.for("oms.codex-harness")] ??= {
  url: process.env.OMS_CODEX_HARNESS_URL,
  capability: process.env.OMS_CODEX_HARNESS_CAPABILITY,
  model: process.env.OMS_CODEX_HARNESS_MODEL,
});
delete process.env.OMS_CODEX_HARNESS_URL;
delete process.env.OMS_CODEX_HARNESS_CAPABILITY;
delete process.env.OMS_CODEX_HARNESS_MODEL;

export default function (pi) {
  const model = JSON.parse(route.model);
  pi.registerProvider("oms-codex", {
    baseUrl: route.url + "/v1",
    ["api" + "Key"]: route.capability,
    api: "openai-responses",
    models: [{
      id: model.id,
      name: model.id + " (ChatGPT subscription)",
      reasoning: true,
      input: ["text", "image"],
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
      contextWindow: model.contextWindow,
      maxTokens: 32768,
    }],
  });
}
