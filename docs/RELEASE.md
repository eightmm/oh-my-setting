# Release process

Releases are cut by pushing a tag that matches the `VERSION` file. The
`release` workflow then gates, checksums, and publishes a GitHub Release.

## Steps

1. Land all changes on `main` and make sure CI is green.
2. Bump `VERSION` (e.g. `0.3.1`) and add a matching `## [0.3.1]` section to
   `CHANGELOG.md`. Commit on `main`.
3. Tag and push:

   ```bash
   git tag -a "v$(cat VERSION)" -m "v$(cat VERSION)"
   git push origin "v$(cat VERSION)"
   ```

The tag push triggers `.github/workflows/release.yml`, which:

- runs `bash scripts/check.sh` (shellcheck + smoke) — a red tree never releases;
- asserts the tag equals `v$(cat VERSION)`;
- generates `SHA256SUMS` (via `scripts/gen-checksums.sh`) and
  `install.sh.sha256`;
- creates the GitHub Release with `install.sh`, `install.sh.sha256`, and
  `SHA256SUMS` attached, using the matching `CHANGELOG.md` section as notes
  (falling back to auto-generated notes).

## Verifying a downloaded install.sh

```bash
curl -fsSLO https://github.com/eightmm/oh-my-setting/releases/download/vX.Y.Z/install.sh
curl -fsSLO https://github.com/eightmm/oh-my-setting/releases/download/vX.Y.Z/install.sh.sha256
sha256sum -c install.sh.sha256
bash install.sh
```

`SHA256SUMS` covers the installer plus every tracked script and the skills
manifest, so a clone can be audited against a release with `sha256sum -c`.

## Channels

- **Stable**: the latest GitHub Release tag.
- **Edge**: `main` (what `install.sh` clones today). Pin to a tag for
  reproducibility.
