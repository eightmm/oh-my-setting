# Spec-First Workflow

Use for new projects, broad features, or unclear requests.

1. Interview before code.
2. Ask only implementation-changing questions.
3. Write compact spec.
4. Confirm spec or resolve blockers.
5. Implement smallest version.
6. Verify against spec.

Minimum spec:

```md
Goal:
Non-goals:
Users:
Scope:
Constraints:
Interface:
Data:
Success:
Verification:
Assumptions:
Open questions:
```

Gate:

- If goal, constraints, success, or verification are unclear: ask.
- If architecture/data/API/security/resource choice is unclear: ask.
- If ambiguity is local and low-risk: state assumption, continue.
