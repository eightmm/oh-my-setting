# Source Reading and Summaries

Use for long documents, source investigations and summaries where missing text
could change the answer. Short, known lookups can stay local.

## Delegate the reading

Use one available native read-only subagent, or the existing OMS `researcher`
route within authorized provider, spending and source-access limits. If no
permitted route is available, read locally; do not install, log in, switch
providers or start another worker to recover. The parent reads its own governing
instructions and applicable skills; a summary does not replace them.

Give a fresh brief with the question, exact sources, requested coverage and
return requirements. No full conversation replay, edits, recursive delegation,
implementation or external messages. Do not duplicate a current complete read.
Read-only is a scope constraint, not a claim of OS sandboxing.

## Verify what was read

- Record source identity/version and expected extent: file lines/bytes, document
  pages or web sections/pagination. If extent is unknown, say so and do not claim
  complete coverage. Keep the same source version across chunks;
  if it changes, reconcile or reread affected ranges before claiming coverage.
- Inspect actual output at every layer, including nested calls and the outer
  aggregator. Larger `max_output_tokens` values and successful downloads do not
  establish completeness. Never use an unnoticed `slice`, head/tail, search
  snippet or summary in place of the requested document.
- Read oversized text in contiguous bounded ranges through EOF, recording which
  ranges returned. A line cap alone cannot bound a giant line; split it safely
  with UTF-8 decoding intact. Parallel reads are useful only while their combined
  returned output fits the receiving tool's budget; otherwise return separately.
- For web pages, read the rendered main content or an equivalent full-text
  representation, following pagination. A loading shell is not the document.
  Identify relevant tables, figures, attachments or collapsed sections that the
  text reader did not expose; inspect them or record the gap.

## Return evidence, then decide

Return a concise summary with source identity, total and covered extent, missing
ranges or inaccessible material, and exact file/line, page/section or URL anchors
for key claims. Preserve exceptions, prohibitions and conflicting evidence; do
not invent missing conditions or report partial reads as complete. Keep a large
report in an authorized artifact and return its location plus a bounded digest.

The parent verifies source identity/coverage and checks decisive passages in the
original before decisions or actions. It need not reread the whole source merely
to repeat the summary. If the summary or handoff is itself truncated, retrieve
the missing ranges from its artifact before using them. A coverage report is
worker evidence, not proof of semantic understanding. Unrecovered gaps remain
explicit, and conclusions depending on them wait. A summary grants no write,
acceptance, publication or spending authority.
