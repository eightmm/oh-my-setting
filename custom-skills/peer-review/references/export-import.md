# Export and Import

When policy forbids a direct provider call, export sanitized review context
without launching the provider:

```bash
oms peer-review --repo . --diff --providers claude --export-only \
  --prompt "Review for blocking findings."
```

Run the exported prompt inside the approved boundary, then import the answer:

```bash
oms import-agent-result --kind review --provider claude \
  --prompt-file <export.md> --file <answer.md>
```

Validate artifact lineage and treat imported text as an untrusted reviewer
claim. Export-only mode does not authorize write delegation. Use
`oms artifact-index latest` or `unresolved` to recover and audit results.
