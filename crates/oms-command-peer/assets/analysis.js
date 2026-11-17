/* Pure comparison and review-state helpers shared by the offline UI and tests. */
"use strict";
(() => {
  const value = (row, name) => typeof row.metrics[name] === "number" && Number.isFinite(row.metrics[name]) ? row.metrics[name] : null;
  const pairKey = (row) => JSON.stringify([row.case_id, row.repeat]);
  function baselineIndex(rows, selector, metric) {
    const result = new Map();
    for (const row of rows) {
      const match = selector.startsWith("variant:") ? row.variant === selector.slice(8) : selector.startsWith("run:") && row.run_id === selector.slice(4);
      if (!match || row.status !== "complete" || value(row, metric) === null) continue;
      const key = pairKey(row);
      result.set(key, result.has(key) ? null : row);
    }
    return result;
  }
  function delta(row, index, metric) {
    const base = index.get(pairKey(row)), observed = value(row, metric);
    if (!base || row.status !== "complete" || observed === null) return null;
    const difference = observed - value(base, metric);
    return Number.isFinite(difference) ? difference : null;
  }
  function groups(rows, metric) {
    const collected = new Map();
    for (const row of rows) {
      if (!collected.has(row.variant)) collected.set(row.variant, { variant: row.variant, total: 0, values: [] });
      const group = collected.get(row.variant); group.total++;
      const n = value(row, metric); if (row.status === "complete" && n !== null) group.values.push(n);
    }
    return [...collected.values()].sort((a, b) => a.variant.localeCompare(b.variant)).map((group) => {
      const values = group.values, count = values.length;
      const mean = count ? values.reduce((sum, n) => sum + n / count, 0) : null;
      // Normalize before squaring to avoid overflow for otherwise finite inputs.
      const scale = count ? Math.max(...values.map(Math.abs)) || 1 : 1;
      const sd = count > 1 ? Math.sqrt(values.reduce((sum, n) => sum + (n / scale - mean / scale) ** 2, 0) / (count - 1)) * scale : null;
      return { variant: group.variant, total: group.total, count, mean: Number.isFinite(mean) ? mean : null, sd: Number.isFinite(sd) ? sd : null, min: count ? Math.min(...values) : null, max: count ? Math.max(...values) : null };
    });
  }
  function createAxis(values) {
    if (!values.length || values.some((n) => typeof n !== "number" || !Number.isFinite(n))) throw new Error("Plot axes require finite numeric values.");
    const min = Math.min(...values), max = Math.max(...values), magnitude = Math.max(Math.abs(min), Math.abs(max)) || 1;
    const lower = min / magnitude, upper = max / magnitude, span = max - min;
    const clamp = (n, lo, hi) => Math.max(lo, Math.min(hi, n));
    return {
      min, max,
      position(n) {
        if (typeof n !== "number" || !Number.isFinite(n)) throw new Error("Plot coordinates must be finite.");
        if (min === max) return .5;
        if (n <= min) return 0;
        if (n >= max) return 1;
        // Opposite finite extremes may have an infinite raw difference. The
        // normalized domain stays within [-1, 1], including subnormal values.
        const fraction = Number.isFinite(span) ? (n - min) / span : (n / magnitude - lower) / (upper - lower);
        return clamp(fraction, 0, 1);
      },
      at(fraction) {
        if (typeof fraction !== "number" || !Number.isFinite(fraction)) throw new Error("Plot tick fractions must be finite.");
        const t = clamp(fraction, 0, 1);
        if (t === 0) return min;
        if (t === 1) return max;
        if (min === max) return min;
        // Same-sign subtraction is safe; opposite-sign weighted terms avoid
        // forming max - min, which can overflow despite two finite endpoints.
        const interpolated = Math.sign(min) === Math.sign(max) ? min + (max - min) * t : min * (1 - t) + max * t;
        return clamp(interpolated, min, max);
      },
    };
  }
  const controlNames = ["search", "variant", "status", "metric", "baseline", "axis-x", "molecule-a", "molecule-b"];
  const reportNames = ["title", "question", "decision", "next"];
  const judgments = new Set(["accept", "reject", "uncertain", "comment"]);
  function annotation(key, item) {
    if (!item || !judgments.has(item.action) || typeof item.comment !== "string" || item.comment.length > 4000) throw new Error("Annotations require a valid judgment and a comment of at most 4,000 characters.");
    return { row_key: key, action: item.action, comment: item.comment };
  }
  function snapshotState(controls, report, state) {
    return { controls: { ...controls }, report: { ...report }, selected: [...state.selected],
      feedback: [...state.feedback].map(([key, item]) => annotation(key, item)), focused: state.focused };
  }
  function restoreState(saved, knownKeys) {
    if (!saved || typeof saved !== "object" || Array.isArray(saved)) throw new Error("Saved view state must be an object.");
    const selected = saved.selected || [], feedback = saved.feedback || [];
    if (!Array.isArray(selected) || !Array.isArray(feedback) || selected.length > 10000 || feedback.length > 10000) throw new Error("Saved view state exceeds the 10,000-case source limit.");
    const controls = {}, report = {}, restored = new Map();
    for (const [key, val] of Object.entries(saved.controls || {})) if (controlNames.includes(key) && typeof val === "string") controls[key] = val;
    for (const [key, val] of Object.entries(saved.report || {})) if (reportNames.includes(key) && typeof val === "string") report[key] = val;
    for (const item of feedback) {
      if (!item || !knownKeys.has(item.row_key)) continue;
      if (restored.has(item.row_key)) throw new Error("Saved view contains duplicate annotations for a case.");
      restored.set(item.row_key, annotation(item.row_key, item));
    }
    return { controls, report, selected: new Set(selected.filter((key) => knownKeys.has(key))),
      feedback: restored, focused: knownKeys.has(saved.focused) ? saved.focused : null };
  }
  function exportFeedback(payload, state, report, scope = "all") {
    if (!["all", "selected"].includes(scope)) throw new Error("Choose all feedback or selected feedback.");
    if (scope === "selected" && !state.selected.size) throw new Error("Select at least one case before exporting selected feedback.");
    const keys = scope === "selected" ? [...state.selected] : [...new Set([...state.feedback.keys(), ...state.selected])];
    if (keys.length > 1000) throw new Error("Feedback supports 1,000 cases per file. Select a smaller batch and use Export selected feedback; all annotations are kept.");
    const events = keys.map((key) => state.feedback.has(key) ? annotation(key, state.feedback.get(key)) : { row_key: key, action: "select", comment: "" });
    const document = { schema: 1, kind: "oms-artifact-feedback", view_id: payload.view_id,
      data_sha256: payload.data_sha256, events, report: { ...report } };
    const json = JSON.stringify(document, null, 2) + "\n";
    if (new TextEncoder().encode(json).length > 1024 * 1024) throw new Error("Feedback exceeds the 1 MiB import limit. Select a smaller batch and use Export selected feedback; comments are kept in full.");
    return json;
  }
  const api = { value, baselineIndex, delta, groups, createAxis, snapshotState, restoreState, exportFeedback };
  if (typeof module === "object" && module.exports) module.exports = api;
  else window.OMSViewAnalysis = api;
})();
