// The only credential in Pi is the capability for this invocation's loopback gateway.
export default function (pi) {
  const model = JSON.parse(process.env.OMS_CODEX_HARNESS_MODEL);
  pi.registerProvider("oms-codex", {
    baseUrl: process.env.OMS_CODEX_HARNESS_URL + "/v1",
    apiKey: process.env.OMS_CODEX_HARNESS_CAPABILITY,
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
