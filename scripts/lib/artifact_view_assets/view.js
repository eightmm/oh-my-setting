/* All record text enters the DOM as textContent. This file also runs offline. */
"use strict";
(() => {
  const $ = (id) => document.getElementById(id);
  const decode = (text) => new TextDecoder().decode(Uint8Array.from(atob(text.trim()), (c) => c.charCodeAt(0)));
  const encode = (text) => { let raw = ""; for (const n of new TextEncoder().encode(text)) raw += String.fromCharCode(n); return btoa(raw); };
  const payload = JSON.parse(decode($("oms-payload").textContent));
  const analysis = window.OMSViewAnalysis;
  const rows = payload.data.rows, byKey = new Map(rows.map((r) => [r.key, r]));
  const metrics = [...new Set(rows.flatMap((r) => Object.keys(r.metrics)))].sort();
  const parameters = [...new Set(rows.flatMap((r) => Object.entries(r.params).filter(([, v]) => typeof v === "number" && Number.isFinite(v)).map(([k]) => k)))].sort();
  const variants = [...new Set(rows.map((r) => r.variant))].sort();
  const runs = [...new Set(rows.map((r) => r.run_id))].sort();
  const state = { page: 0, selected: new Set(), feedback: new Map(), focused: null };
  let filtered = rows, viewers = [], restoring = false, camerasLinked = false, baselineIndex = new Map();
  const pageSize = 100;
  function node(tag, text, className) { const el = document.createElement(tag); if (text !== undefined) el.textContent = text; if (className) el.className = className; return el; }
  function options(id, values, first) { const el = $(id); el.replaceChildren(); if (first) { const o = node("option", first); o.value = ""; el.append(o); } for (const v of values) { const o = node("option", typeof v === "string" ? v : v.label); o.value = typeof v === "string" ? v : v.value; el.append(o); } }
  function number(value) { return typeof value === "number" && Number.isFinite(value) ? value.toLocaleString("en", { maximumSignificantDigits: 6 }) : "—"; }
  function metric(row) { return analysis.value(row, $("metric").value); }
  function delta(row) { return analysis.delta(row, baselineIndex, $("metric").value); }
  function deltaClass(value) { const direction = payload.data.metric_directions[$("metric").value]; if (value === null || value === 0 || !direction) return "num"; return "num " + ((direction === "min" ? value < 0 : value > 0) ? "better" : "worse"); }
  function notice(message, error = false) { $("notice").textContent = message; $("notice").className = error ? "notice-error" : ""; }
  function table(headers) { const t = node("table"), h = node("thead"), tr = node("tr"), body = node("tbody"); headers.forEach((x) => tr.append(node("th", x))); h.append(tr); t.append(h, body); return [t, body]; }
  function update() {
    const query = $("search").value.toLocaleLowerCase(), variant = $("variant").value, status = $("status").value;
    filtered = rows.filter((r) => (!variant || r.variant === variant) && (!status || r.status === status) && (!query || JSON.stringify([r.run_id, r.case_id, r.variant, r.params]).toLocaleLowerCase().includes(query)));
    baselineIndex = analysis.baselineIndex(rows, $("baseline").value, $("metric").value);
    state.page = Math.max(0, Math.min(state.page, Math.ceil(filtered.length / pageSize) - 1));
    $("count").textContent = `${filtered.length} of ${rows.length} rows · ${state.selected.size} selected`;
    $("export-selected-feedback").disabled = !state.selected.size;
    const direction = payload.data.metric_directions[$("metric").value];
    $("direction").textContent = direction ? `${$("metric").value}: ${direction === "min" ? "lower" : "higher"} is better. Δ = row − same-case baseline.` : "Metric direction unspecified. Δ = row − same-case baseline.";
    renderGroups(); renderRows(); renderPlot();
  }
  function renderGroups() {
    const [t, body] = table(["Variant", "Complete / total", "Mean", "Sample SD", "Min", "Max"]);
    for (const group of analysis.groups(filtered, $("metric").value)) {
      const tr = node("tr"); [group.variant, `${group.count} / ${group.total}`, number(group.mean), number(group.sd), number(group.min), number(group.max)].forEach((v, i) => tr.append(node("td", v, i > 1 ? "num" : ""))); body.append(tr);
    }
    $("groups").replaceChildren(t);
  }
  function visibleRows() { return filtered.slice(state.page * pageSize, (state.page + 1) * pageSize); }
  function renderRows() {
    const [t, body] = table(["Select", "Case", "Run", "Variant", "Repeat", "Status", $("metric").value || "Metric", "Δ baseline"]);
    for (const row of visibleRows()) {
      const tr = node("tr"), choice = node("input"), td = node("td"); choice.type = "checkbox"; choice.checked = state.selected.has(row.key); choice.setAttribute("aria-label", `Select ${row.run_id} / ${row.case_id} / ${row.repeat}`);
      choice.addEventListener("change", () => { if (choice.checked) state.selected.add(row.key); else state.selected.delete(row.key); update(); }); td.append(choice); tr.append(td);
      const link = node("button", row.case_id, "row-link"), cell = node("td"); link.addEventListener("click", () => focus(row.key)); cell.append(link); tr.append(cell);
      [row.run_id, row.variant, String(row.repeat), row.status, number(metric(row))].forEach((v, i) => tr.append(node("td", v, i === 4 ? "num" : "")));
      const d = delta(row); tr.append(node("td", d === null ? "—" : (d > 0 ? "+" : "") + number(d), deltaClass(d))); body.append(tr);
    }
    $("rows").replaceChildren(t); $("page").textContent = `${filtered.length ? state.page + 1 : 0} / ${Math.ceil(filtered.length / pageSize)}`;
    $("previous").disabled = !state.page; $("next-page").disabled = (state.page + 1) * pageSize >= filtered.length;
  }
  function renderPlot() {
    const ns = "http://www.w3.org/2000/svg", svg = document.createElementNS(ns, "svg"); svg.setAttribute("viewBox", "0 0 640 250");
    const axis = $("axis-x").value, xValue = (r, i) => axis.startsWith("param:") ? r.params[axis.slice(6)] : axis.startsWith("metric:") ? r.metrics[axis.slice(7)] : i;
    const all = filtered.map((r, i) => ({ row: r, x: xValue(r, i), y: metric(r) })).filter((p) => typeof p.x === "number" && Number.isFinite(p.x) && p.y !== null), points = all.slice(0, 1500);
    function draw(tag, attrs, text) { const el = document.createElementNS(ns, tag); for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, String(v)); if (text !== undefined) el.textContent = text; svg.append(el); return el; }
    if (!points.length) { draw("text", { x: 32, y: 120, fill: "#607471", "font-size": 13 }, "No numeric values match these filters."); }
    else {
      const xAxis = analysis.createAxis(points.map((p) => p.x)), yAxis = analysis.createAxis(points.map((p) => p.y));
      const x = (v) => 68 + xAxis.position(v) * 544, y = (v) => 208 - yAxis.position(v) * 180;
      for (const fraction of yAxis.min === yAxis.max ? [.5] : [0, .25, .5, .75, 1]) { const py = 208 - fraction * 180; draw("line", { x1: 68, y1: py, x2: 612, y2: py, stroke: "#e2eae5" }); draw("text", { x: 60, y: py + 4, "text-anchor": "end", fill: "#607471", "font-size": 10 }, number(yAxis.at(fraction))); }
      for (const fraction of xAxis.min === xAxis.max ? [.5] : [0, 1]) draw("text", { x: 68 + fraction * 544, y: 235, "text-anchor": fraction === 1 ? "end" : fraction === .5 ? "middle" : "start", fill: "#607471", "font-size": 10 }, number(xAxis.at(fraction)));
      const colors = ["#117766", "#bc6c36", "#656ab3", "#b05077", "#4e839d", "#788c3e"];
      for (const point of points) { const circle = draw("circle", { cx: x(point.x), cy: y(point.y), r: state.selected.has(point.row.key) ? 6 : 4, fill: colors[variants.indexOf(point.row.variant) % colors.length], opacity: .78, tabindex: 0, role: "button", "aria-label": `${point.row.case_id}: x ${number(point.x)}, y ${number(point.y)}` }); const title = document.createElementNS(ns, "title"); title.textContent = `${point.row.run_id} · ${point.row.case_id} · ${point.row.variant}\nx=${number(point.x)}, y=${number(point.y)}`; circle.append(title); circle.addEventListener("click", () => focus(point.row.key)); circle.addEventListener("keydown", (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); focus(point.row.key); } }); }
    }
    $("plot").replaceChildren(svg); $("plot-note").textContent = `${points.length} points${all.length > points.length ? ` (first 1,500 of ${all.length}; table and exports retain all)` : ""}. X: ${axis || "filtered row index"}. Y: ${$("metric").value || "none"}. Click a point to inspect its case.`;
  }
  function focus(key) {
    const row = byKey.get(key); if (!row) return; state.focused = key; $("detail").hidden = false; $("detail-empty").hidden = true;
    $("detail-title").textContent = `${row.case_id} / ${row.variant} / repeat ${row.repeat}`;
    $("detail-data").textContent = JSON.stringify({ run_id: row.run_id, row_key: row.key, status: row.status, metrics: row.metrics, params: row.params }, null, 2);
    const feedback = state.feedback.get(key); $("judgment").value = feedback && ["accept", "reject", "uncertain"].includes(feedback.action) ? feedback.action : ""; $("comment").value = feedback ? feedback.comment : "";
    $("feedback-status").textContent = feedback ? "Recorded locally in this view; export to import into OMS." : "";
    options("artifact", row.artifacts.map((a, i) => ({ value: String(i), label: a.label || a.path })), "Select artifact");
    if (row.artifacts.length) $("artifact").value = "0"; showArtifact();
  }
  function showArtifact() {
    const row = byKey.get(state.focused), a = row && row.artifacts[$("artifact").value], box = $("artifact-preview"); box.replaceChildren(); if (!a) return;
    box.append(node("p", `${a.path} · ${a.sha256 || "digest unavailable"}`, "muted"));
    if (a.image) { const img = node("img"); img.src = a.image; img.alt = a.label || a.path; box.append(img); }
    else if (a.text !== undefined) box.append(node("pre", a.text));
    else box.append(node("p", a.availability || "Reference only."));
  }
  const molecules = rows.flatMap((r) => r.artifacts.filter((a) => ["pdb", "sdf", "mol2", "xyz"].includes(a.format) && a.text !== undefined).map((a) => ({ row: r, artifact: a })));
  function molecule(side) {
    const index = $("molecule-" + side).value, item = index === "" ? null : molecules[Number(index)], target = $("molecule-view-" + side), meta = $("molecule-meta-" + side);
    if (viewers[side]) { viewers[side].clear(); viewers[side].render(); }
    meta.textContent = item ? `Frame: ${item.artifact.frame || "unspecified"}. Alignment: ${item.artifact.alignment || "not supplied"}. SHA-256: ${item.artifact.sha256}` : "";
    if (!item || !window.$3Dmol) return;
    try {
      if (!viewers[side]) viewers[side] = window.$3Dmol.createViewer(target, { backgroundColor: "#f3f7f5", antialias: true });
      const viewer = viewers[side], other = viewers[side === "a" ? "b" : "a"]; viewer.addModel(item.artifact.text, item.artifact.format); viewer.setStyle({}, { stick: { radius: .14 }, cartoon: { color: "spectrum" } }); viewer.zoomTo(); viewer.render();
      if (other && !camerasLinked) { viewer.linkViewer(other); other.linkViewer(viewer); camerasLinked = true; }
      $("molecule-status").textContent = "Offline 3Dmol.js viewer loaded. Drag to rotate, scroll to zoom; paired cameras are linked.";
    } catch (error) { $("molecule-status").textContent = "Molecule rendering unavailable: " + String(error.message || error); }
  }
  function report() { return Object.fromEntries(["title", "question", "decision", "next"].map((k) => [k, $("report-" + k).value])); }
  function download(name, content, type) { const url = URL.createObjectURL(new Blob([content], { type })), link = node("a"); link.href = url; link.download = name; document.body.append(link); link.click(); link.remove(); setTimeout(() => URL.revokeObjectURL(url), 1000); notice("Exported " + name); }
  function safeAction(action) { return () => { try { action(); } catch (e) { notice(e.message || String(e), true); } }; }
  function markdown() {
    const r = report(), escape = (v) => String(v).replace(/\|/g, "\\|").replace(/[\r\n]/g, " "), lines = [`# ${r.title || payload.title}`, "", `Source view: ${payload.view_id}`, `Source SHA-256: ${payload.data_sha256}`, "", "## Question", "", r.question, "", "## Decision and evidence", "", r.decision, "", "## Next experiment / instruction", "", r.next, "", "## Comparison", "", `Metric: ${$("metric").value || "none"}; direction: ${payload.data.metric_directions[$("metric").value] || "unspecified"}.`, `Baseline run: ${$("baseline").value || "none"}. Filtered rows: ${filtered.length} / ${rows.length}.`, `Filters: search=${JSON.stringify($("search").value)}, variant=${JSON.stringify($("variant").value)}, status=${JSON.stringify($("status").value)}.`, "", "| Run | Case | Variant | Repeat | Status | Metric | Δ baseline |", "| --- | --- | --- | ---: | --- | ---: | ---: |"];
    for (const row of filtered) lines.push("| " + [row.run_id, row.case_id, row.variant, row.repeat, row.status, number(metric(row)), number(delta(row))].map(escape).join(" | ") + " |");
    lines.push("", "## Selected cases and annotations", "");
    const keys = new Set([...state.selected, ...state.feedback.keys()]); for (const key of keys) { const row = byKey.get(key), f = state.feedback.get(key); lines.push(`- ${escape(row.run_id)} / ${escape(row.case_id)} / ${escape(row.variant)} / repeat ${row.repeat} (${key}): ${f ? f.action : "select"}${f && f.comment ? " — " + escape(f.comment) : ""}`); }
    return lines.join("\n") + "\n";
  }
  function snapshot() { return analysis.snapshotState(Object.fromEntries(["search", "variant", "status", "metric", "baseline", "axis-x", "molecule-a", "molecule-b"].map((id) => [id, $(id).value])), report(), state); }
  function saveView() { $("oms-saved-state").textContent = encode(JSON.stringify(snapshot())); const copy = document.documentElement.cloneNode(true); for (const id of ["molecule-view-a", "molecule-view-b"]) copy.querySelector("#" + id).replaceChildren(); for (const el of copy.querySelectorAll("script[data-oms-vendor]")) el.remove(); const html = "<!doctype html>\n" + copy.outerHTML; if (new TextEncoder().encode(html).length > 24 * 1024 * 1024) throw new Error("Saved HTML exceeds 24 MiB. Export the report and feedback separately or build a smaller source view."); download(payload.view_id + ".html", html, "text/html;charset=utf-8"); }
  options("variant", variants, "All variants"); options("metric", metrics); options("baseline", [...variants.map((k) => ({ value: "variant:" + k, label: "Variant: " + k })), ...runs.map((k) => ({ value: "run:" + k, label: "Run: " + k }))], "No baseline"); options("axis-x", [...parameters.map((k) => ({ value: "param:" + k, label: "Parameter: " + k })), ...metrics.map((k) => ({ value: "metric:" + k, label: "Metric: " + k }))], "Row index");
  for (const side of ["a", "b"]) { options("molecule-" + side, molecules.map((m, i) => ({ value: String(i), label: `${m.row.run_id} / ${m.row.case_id} / ${m.artifact.label || m.artifact.path}` })), "Select structure"); $("molecule-" + side).addEventListener("change", () => molecule(side)); }
  $("title").textContent = payload.title; $("report-title").value = payload.title; $("source").textContent = `${payload.view_id} · SHA-256 ${payload.data_sha256}`;
  for (const id of ["search", "variant", "status", "metric", "baseline", "axis-x"]) $(id).addEventListener(id === "search" ? "input" : "change", () => { state.page = 0; update(); });
  $("clear").addEventListener("click", () => { ["search", "variant", "status", "baseline", "axis-x"].forEach((id) => { $(id).value = ""; }); state.page = 0; update(); });
  $("previous").addEventListener("click", () => { state.page--; update(); }); $("next-page").addEventListener("click", () => { state.page++; update(); });
  $("select-page").addEventListener("click", () => { visibleRows().forEach((r) => state.selected.add(r.key)); update(); }); $("clear-selection").addEventListener("click", () => { state.selected.clear(); update(); });
  $("artifact").addEventListener("change", showArtifact);
  $("record-comment").addEventListener("click", () => { if (!state.focused) return; const action = $("judgment").value || "comment", comment = $("comment").value; if (!comment && action === "comment") { state.feedback.delete(state.focused); $("feedback-status").textContent = "Annotation removed from this view."; return; } state.feedback.set(state.focused, { row_key: state.focused, action, comment }); $("feedback-status").textContent = "Recorded locally. Export feedback to import into OMS."; });
  $("export-feedback").addEventListener("click", safeAction(() => download(payload.view_id + "-feedback.json", analysis.exportFeedback(payload, state, report(), "all"), "application/json")));
  $("export-selected-feedback").addEventListener("click", safeAction(() => download(payload.view_id + "-selected-feedback.json", analysis.exportFeedback(payload, state, report(), "selected"), "application/json")));
  $("export-report").addEventListener("click", safeAction(() => download(payload.view_id + "-report.md", markdown(), "text/markdown;charset=utf-8")));
  $("save-view").addEventListener("click", safeAction(saveView));
  try {
    if ($("oms-saved-state").textContent.trim()) {
      const saved = analysis.restoreState(JSON.parse(decode($("oms-saved-state").textContent)), new Set(byKey.keys()));
      for (const [id, value] of Object.entries(saved.controls)) $(id).value = value;
      for (const [key, value] of Object.entries(saved.report)) $("report-" + key).value = value;
      state.selected = saved.selected; state.feedback = saved.feedback; state.focused = saved.focused; restoring = true;
    }
  } catch (error) { notice("Saved layout could not be restored; source records are unchanged.", true); }
  const vendor = $("oms-molecule-library").textContent.trim();
  if (vendor) { const script = node("script"); script.dataset.omsVendor = "1"; script.textContent = decode(vendor); document.head.append(script); }
  $("molecule-status").textContent = window.$3Dmol ? "Local 3Dmol.js asset ready. Select a pair of structures." : "Build with --molecule-js /path/to/3Dmol-min.js to enable offline 3D. Molecular text remains available under case artifacts.";
  update(); if (state.focused) focus(state.focused); if (restoring && window.$3Dmol) { molecule("a"); molecule("b"); }
})();
