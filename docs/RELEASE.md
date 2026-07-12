# Release process

Releases are cut by pushing a tag that matches the `VERSION` file and Codex
plugin version. The `release` workflow then gates, builds a tag-pinned
installer, verifies checksums, and publishes a GitHub Release.

## Steps

1. Land all changes on `main` and make sure CI is green.
2. Bump `VERSION` (e.g. `0.4.0`) and the plugin manifest version, then add a
   matching `## [0.4.0]` section to
   `CHANGELOG.md`. Commit on `main`.
3. Tag and push:

   ```bash
   git tag -a "v$(cat VERSION)" -m "v$(cat VERSION)"
   git push origin "v$(cat VERSION)"
   ```

The tag push triggers `.github/workflows/release.yml`, which:

- runs `bash scripts/check.sh` (shellcheck + smoke) — a red tree never releases;
- asserts the tag equals `v$(cat VERSION)`, the plugin version matches, and a
  matching changelog section exists;
- rewrites the source installer's single `edge` placeholder to the exact tag;
- generates and then verifies `SHA256SUMS` (via `scripts/gen-checksums.sh`) and
  the generated installer's `install.sh.sha256`;
- creates the GitHub Release with `install.sh`, `install.sh.sha256`, and
  `SHA256SUMS` attached, using the non-empty matching `CHANGELOG.md` section as
  notes.

## Verifying a downloaded install.sh

```bash
curl -fsSLO https://github.com/eightmm/oh-my-setting/releases/download/vX.Y.Z/install.sh
curl -fsSLO https://github.com/eightmm/oh-my-setting/releases/download/vX.Y.Z/install.sh.sha256
sha256sum -c install.sh.sha256
bash install.sh
```

`SHA256SUMS` covers installers, agent rules, tracked shell/Python scripts,
custom skills and references, project templates, plugin metadata, the skills
manifest, prompts, roles, the `oms` dispatcher, and
`VERSION`. From the repository root, verify it portably with:

```bash
scripts/gen-checksums.sh --strict > SHA256SUMS
scripts/gen-checksums.sh --verify SHA256SUMS
```

## Channels

- **Stable**: the latest GitHub Release asset. The published `install.sh`
  embeds that exact tag and checks it out detached, so it never advances by
  accident.
- **Edge**: the source `install.sh` embeds `edge`; it checks out and
  fast-forwards the origin default branch.
- **Explicit pin**: `--ref v0.4.0`, `--ref BRANCH`, or `--ref COMMIT` overrides
  the embedded channel. `OH_MY_SETTING_REF` is the environment equivalent.

`--ref` wins over the environment, which wins over the installer-embedded
default. Switching a managed checkout from a pin to `--ref edge` restores the
origin default branch. Invalid or unresolved refs fail before linking files.

The source manifest and the generated installer have separate checksums:
`SHA256SUMS` audits the tagged source tree; `install.sh.sha256` audits the
tag-pinned release asset.
