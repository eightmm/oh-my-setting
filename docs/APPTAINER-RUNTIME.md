# Optional Apptainer runtime

`oms runtime backend run apptainer` runs an explicitly selected local SIF through
the existing OMS process supervisor. The `isolated`, `trusted-local`, and `remote`
profiles keep their existing meanings. Apptainer is optional and must already be
installed on Linux, including a suitable WSL2 host. macOS reports this profile as
not ready.

```bash
oms runtime backend describe apptainer
oms runtime backend check apptainer --image /opt/images/cpu.sif
oms runtime backend run apptainer --image /opt/images/cpu.sif \
  --timeout-seconds 300 --log results/check.log --receipt results/check.json \
  -- python -c 'print("ready")'
```

The image must be a nonempty regular local file, at most 64 GiB. Symlink image
files, device files, directories, and image URIs refuse. OMS checks the installed
runtime's version and required options and asks `apptainer sif header` to validate
the SIF header. It never pulls, builds, or downloads an image. `--image` is required;
`OMS_ISOLATED_IMAGE` is not used for this profile.

## Requested execution policy

OMS requests `--containall --cleanenv --no-home --no-eval --userns`, disables the
automatic `hostfs`, `bind-paths`, and `cwd` mounts, drops all capabilities, and uses
`--no-privs`. It does not request fakeroot or switch to privileged execution when
the host refuses a user namespace. The image filesystem is left read-only.

The repository is bound read-only at its physical path. An explicit `--worktree`
selects the only writable project bind and the payload working directory:

```bash
oms runtime backend run apptainer --image /opt/images/cpu.sif \
  --worktree /srv/project-worktree -- python train.py
```

Selecting `--worktree .` explicitly permits writes to the execution repository.
Without that option, the repository stays read-only. OMS refuses ambiguous bind
delimiters, a filesystem root, the whole host home, or an ancestor of that home.
Temporary directories and the contained home follow Apptainer's containment
behavior; they are not additional writable project mounts managed by OMS.

Networking defaults to `--net --network none`. `--allow-network` explicitly
requests host networking. A runtime refusal never triggers a second launch with
networking or other restrictions removed.

The launcher starts with a cleared environment. Minimal host configuration and a
validated local user session are allowed for the runtime itself. Ambient
`APPTAINER*` and `SINGULARITY*` controls are excluded. `--inherit-env NAME` selects
additional container values; names must be identifiers, must pass the existing
sensitive-value filter, and cannot override reserved launcher controls or `HOME`.
Selected values use `APPTAINERENV_` with `--no-eval`, so OMS does not interpolate a
shell command. The container receives `HOME=/tmp`; image-defined environment
still applies.

GPU integration is unimplemented. No NVIDIA, ROCm, CDI, or other accelerator
options are requested. This increment covers CPU execution and Apptainer's
default contained device exposure; it makes no GPU or hardware-isolation claim.

## Resources and readiness

The existing `--memory-mb`, `--cpus`, and `--pids-limit` values are forwarded as Apptainer
resource requests. These include the existing defaults of 4096 MiB, 2 CPUs, and
512 PIDs. OMS does not silently drop requests if cgroups are unavailable.
`--tmpfs-mb` and `--adapter` are unsupported for this profile and refuse.

Rootless resource control requires suitable cgroups v2 and systemd delegation.
OMS accepts `XDG_RUNTIME_DIR` only as an absolute, private directory owned by the
current user, and accepts `DBUS_SESSION_BUS_ADDRESS` only as
`unix:path=<XDG_RUNTIME_DIR>/bus`, pointing to an owned Unix socket. Remote,
abstract, or additional D-Bus address forms are unsupported. Missing or invalid
session inputs make rootless readiness fail. Host root can run without these
session variables; supplied session variables still require validation.

A successful check establishes executable, option, image-header, and session-input
readiness. It does not prove namespace availability, controller delegation, mount
enforcement, or administrator configuration. Apptainer may still refuse execution.
OMS preserves that exit status and bounded output without a weaker retry.

## Evidence and limits

The existing supervisor owns the process, wall timeout, cancellation, bounded log,
and exclusive receipt publication. The receipt names the actual profile, chosen
network and mounts, inherited key names, runtime version, and image observation.
`launcher_exit` is the Apptainer process outcome; it does not establish that the
payload started. `exit` is the final OMS outcome.

The image SHA256 is streamed before launch and checked again afterward. File
identity and metadata changes also refuse acceptance. Observed drift changes the
OMS outcome to 125 while retaining the launcher exit, log, and image error. This
is an observation of a local file, not an immutable image pin, signature check, or
proof that a concurrently changing pathname could never be opened. Operators must
keep the selected image stable during execution.

Each hash reads at most the original bounded size plus one byte and checks a
120-second observation budget between reads. A blocking filesystem read cannot be
preempted by that check. The payload wall timeout is separate from preflight and
image observation.

Resource enforcement is recorded as unknown, with the requested flags listed.
The inherited `tmpfs_mb` receipt field is explicitly unsupported and unenforced.
Namespace, kernel, device, and cgroup isolation are not attested merely because an
argument was supplied. Fake-runtime tests exercise OMS invocation and supervision;
they are not evidence of real container or hardware enforcement.

## Upstream contracts

- [Apptainer exec options](https://apptainer.org/docs/user/latest/cli/apptainer_exec.html)
- [Bind paths and containment](https://apptainer.org/docs/user/latest/bind_paths_and_mounts.html)
- [Environment and evaluation](https://apptainer.org/docs/user/latest/environment_and_metadata.html)
- [Network virtualization](https://apptainer.org/docs/user/latest/networking.html)
- [SIF header command](https://apptainer.org/docs/user/latest/cli/apptainer_sif_header.html)
- [Resource limits and cgroups](https://apptainer.org/docs/user/latest/cgroups.html)
- [Rootless session requirements in Apptainer's cgroups package](https://pkg.go.dev/github.com/apptainer/apptainer/internal/pkg/cgroups#CanUseCgroups)
