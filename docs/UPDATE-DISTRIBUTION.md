# Native update distribution

OMS can prepare an exact update separately from applying it. Preparation may
build locally or import a native payload built on another host. The receiving
installation keeps its Git source checkout and the existing rollback-capable
installation transaction.

A regular `oms update` or `oms auto-update apply` also selects an inherited
ref from the install receipt after acquiring the lifecycle and receipt locks.
If another lifecycle operation changes that receipt while the caller waits,
the apply uses the new saved ref. An explicit `--ref` or reviewed release
selection still takes precedence. Scheduled apply failures report the locked
selection and resolved target when available.

## Prepare, inspect, apply

```bash
oms update --prepare --ref edge
oms update --status --json
oms update --apply-prepared
```

`--prepare` resolves the selected ref once, validates the incoming source and
payload, and records their identities. Without `--ref`, it reads the saved ref
from the install receipt while holding the lifecycle and receipt locks.
Preparation rejects group- or world-writable directories from the install
checkout root through the staged payload. It may fetch Git objects and build
dependencies, but leaves the current checkout, installation receipt, managed
configuration, frontdoor and provider versions unchanged. Active workers may
continue while preparation runs. Lifecycle mutations remain serialized.

`--apply-prepared` selects the saved commit without fetching a new ref or
resolving it again. It rejects a changed base commit, replaced source directory, changed
installation receipt, missing payload or changed payload bytes. It rechecks
the existing worker/call fences after snapshots and before the live checkout,
then runs the normal incoming installer and doctor. Provider refresh remains a
separate phase after core commit; `--no-tools` suppresses that phase.

A busy apply preserves the candidate for explicit retry. `prepared` describes
verified payload availability, not permission to activate while workers run.
This last fence check does not provide atomic admission across every provider
session: new sessions and activation still need coordination by the operator.
The existing daily updater does not automatically consume this preparation or
register an idle-time retry.

`--prepare`, `--apply-prepared`, `--status`, `--plan`, `--check`, `--rollback`
and `--probe-agy-surfaces` are mutually exclusive. `--status` accepts only
`--json`; applying a prepared candidate accepts no replacement ref or bundle.
Preparation does not accept provider-refresh or doctor-suppression switches.

## Read preparation status

`oms update --status [--json]` never fetches, repairs a journal or changes files.

| Status | Meaning and next action |
|---|---|
| `none` | No saved preparation. Run `--prepare`. |
| `incomplete` | A preparation journal exists. Wait for the running preparation, or run `--prepare` again after interruption. |
| `prepared` | The base and all staged payload bytes match. Apply when existing worker/call fences permit it. |
| `stale` | The installation or source identity changed. Prepare again. |
| `missing_payload` | The selected cache directory is absent. Prepare again. |
| `invalid_payload` | Payload integrity or safe-file checks failed. Inspect the reported error before preparing again. |

Malformed or unsafe preparation metadata returns a nonzero error. JSON includes
the current commit, target commit, selected ref, preparation time, reason and,
when available, manifest digest. A ready candidate includes its apply command.

## Build once, import on another host

On a trusted build host, check out the intended commit with a clean working tree:

```bash
scripts/package-native-release.sh --output /tmp/oms-native.tar.gz
```

The producer reuses the commit's verified staged payload when available;
otherwise it invokes the existing native build. It validates the source and
payload, packages the fixed native file inventory, and prints the archive's
SHA-256, source commit and host platform. An existing output is never replaced.
Keep the output outside tracked source paths so it does not dirty the checkout.

Transfer the archive through your normal distribution mechanism and obtain its
SHA-256 from an independently trusted source. On an installation that supports
the new flags:

```bash
oms update --prepare --ref <full-commit-sha> \
  --from /path/to/oms-native.tar.gz --sha256 <trusted-sha256>
oms update --status --json
oms update --apply-prepared --no-tools
```

Replace the angle-bracket placeholders before running these commands. A full
commit SHA is 40 lowercase hexadecimal characters; the archive SHA-256 is 64.
With a full SHA and a bundle, preparation requires that commit to exist in the
local Git repository and skips the explicit fetch step. An absent commit
produces an error. For offline use, all required source objects must also be
local: this interface does not disable Git's lazy object fetching in a partial
clone. With a named ref such as `edge`, preparation still fetches to resolve it.
The bundle does not carry source files or Git objects. Import and later apply
reuse the native payload without Cargo compilation on the receiving host.

Apply does not fetch a new ref. Provider refresh, when enabled, retains its
normal behavior; these flags are not a general network-isolation mechanism.

### Bundle contract and trust

The gzip-compressed tar archive contains `bundle.json` and `payload/`. Metadata
uses schema 1, kind `oms-native-payload`, the full `source_commit`, and a
`platform` of `linux-amd64`, `linux-arm64`, `darwin-amd64` or `darwin-arm64`.

The payload contains `oms`, `oms-mcp-peer-owner`, `oms-run-bounded-helper`,
`oms-state-queue-worker`, `oms-graph-python-helper`, `manifest.json` and
`oms.sha256`. `oms-supervisor` is accepted when declared and present. The
manifest records the source commit and each executable's SHA-256.

The importer hashes a bounded snapshot before executing anything from it. It
uses the existing bounded tar extractor, rejects unsafe paths and file types,
checks the schema, exact source commit and host platform, and verifies every
artifact, sidecar and allowed filename. The incoming binary must also validate
its embedded source inventory against an isolated checkout of that commit.

The supplied archive digest is an explicit trust input. It is not a publisher
signature or a claim that CI passed. A digest downloaded from the same untrusted
location as an archive does not establish publisher identity. Existing staged
bytes must match the imported payload; an import never silently replaces a
conflicting cache at the same commit.

Platform identity covers kernel family and CPU architecture, not every runtime
ABI or CPU feature. Build Linux releases against the supported Ubuntu 20.04
floor and macOS releases for their intended deployment targets; avoid
host-specific CPU optimizations for distributed builds. This command does not
cross-compile or attest compatibility with all supported hosts. Musl Linux and
native Windows are not supported.

## Recovery and result boundaries

Preparation has a private receipt and a separate recovery journal under the
OMS configuration directory's `native-preparation/`. Recovering preparation
cannot recover or roll back a live installation transaction. A pending live
update must be recovered before starting another preparation. Update, install,
link and doctor repair resolve interrupted preparation before adopting its
cached payload, so a later recovery cannot remove newly adopted cache files.

Applying a prepared candidate uses the existing core transaction and rollback
evidence. Its preparation receipt is consumed as part of successful core
commit. A failed core transaction restores it when rollback completes. If
provider refresh fails after core commit, the command keeps its failure exit
code and, with `--json`, reports `partial`: `core=updated`, `tools=failed`. Core remains
installed, and its consumed preparation cannot trigger a duplicate apply.
Scheduled updates also preserve this distinction in their status record.

A regular update can install the same target independently. The old preparation
then becomes `stale` and remains available for inspection until replaced by a
new preparation. There is no separate preparation garbage-collection command.

## Scope and follow-up work

This interface adds explicit preparation and local native-bundle distribution.
The current scheduler, ref-selection defaults and `oms land` CI requirements
retain their existing contracts. Native update does not query GitHub Actions;
payload integrity and doctor success do not attest test-suite success. See
[Install verification policy](INSTALL.md#verification-policy).

Further work can add a signed release feed, automatic downloads, a complete
source-independent runtime bundle, atomic admission across all session
producers, an explicit idle-apply policy, unified release-channel selection and
fleet rollout. Those mechanisms are not provided by this local bundle format.
