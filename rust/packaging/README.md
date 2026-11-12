# Ubuntu 20.04 native build recipe

This prepares a GNU/glibc build on the supported Linux floor. It does not
install OMS or establish macOS, WSL2, or full lifecycle support. The build
image provisions Ubuntu's native compiler, linker, headers and inspection
tools; the staged Rust compiler, Cargo, build scripts and procedural macros
all run inside that image. Do not supply host C libraries, linkers, object
files, Cargo configuration, or previously built target artifacts.

## Inputs and image

The owner must first integrate and review a consistent `rust/Cargo.lock` and
workspace. Choose a trusted Ubuntu 20.04 base for the intended architecture.
Set `BASE` to an immutable registry manifest digest reference when available,
or to a local tag verified against the expected image config ID. BuildKit's
`FROM` needs the tag or manifest reference, not the config ID itself. A Docker
image config ID is not a registry manifest digest. Record the base reference, image
ID, architecture, package versions, source revision and lockfile hash with the
results. Apt repositories are mutable: pin or archive the resolved packages and
repository metadata for an independently repeatable image; this recipe alone
does not promise bit-identical builds. Provisioning the image may use network.

For an existing local image, the owner can tag it and verify the tag before
building. Replace the placeholder with the full independently inspected config
ID. Do not substitute a registry manifest digest for that ID in the equality
check. Alternatively set `BASE` to the reviewed `repository@sha256:...`
manifest reference already available locally.

```sh
set -e
EXPECTED_ID='sha256:REPLACE_WITH_INSPECTED_CONFIG_ID'
BASE='oms-ubuntu20-base:reviewed'
docker tag "$EXPECTED_ID" "$BASE"
test "$(docker image inspect "$BASE" --format '{{.Id}}')" = "$EXPECTED_ID"
docker image inspect "$BASE" --format '{{.Id}} {{.Os}}/{{.Architecture}}'
docker build --pull=false --build-arg "BASE_IMAGE=$BASE" \
  -f rust/packaging/Dockerfile.ubuntu20 -t oms-ubuntu20-builder rust/packaging
docker image inspect oms-ubuntu20-builder --format '{{.Id}} {{.Os}}/{{.Architecture}}'
docker run --rm --network none oms-ubuntu20-builder dpkg-query -W
```

Prepare a fresh, owner-reviewed bundle directory with precisely these inputs:

* `rust/`: the complete reviewed workspace source and current lockfile,
  including workspace member manifests and sources, but no `target/`, `.cargo/`
  configuration, credentials, or unrelated repository data.
* `toolchain/`: the complete existing Rust 1.99.1 native toolchain directory,
  including its `bin`, `lib`, and sysroot, for the same architecture. Use its
  real Cargo and rustc executables, not rustup shims. Check for symlinks that
  escape the bundle and resolve them before staging.
* `cache/registry/` and `cache/git/`: a separately prepared, inspected cache
  containing only the locked dependency closure, including required Git
  checkouts and objects. Exclude unrelated private sources, Cargo configuration,
  credentials, provider homes, and machine-private data. Inspect necessary Git
  repository configuration and objects for unrelated history, private material,
  remote credentials, hooks, or external references before staging.

For example, from the reviewed repository root, set the three source paths and
a new bundle path, then stage and review the result. This uses GNU tar on the
Linux build host. The source tree must already be clean of unrelated data;
the exclusions do not replace that review.

```sh
set -e
set -o pipefail
BUNDLE=/absolute/path/to/new-bundle
TOOLCHAIN=/absolute/path/to/real-toolchain
CACHE=/absolute/path/to/reviewed-lock-scoped-cache
mkdir "$BUNDLE"
mkdir "$BUNDLE/rust" "$BUNDLE/toolchain" "$BUNDLE/cache"
tar -C rust --exclude=target --exclude=.cargo -cf - . | tar -C "$BUNDLE/rust" -xf -
cp -a "$TOOLCHAIN"/. "$BUNDLE/toolchain"/
cp -a "$CACHE/registry" "$CACHE/git" "$BUNDLE/cache"/
find "$BUNDLE" -type l -print
git rev-parse HEAD
sha256sum "$BUNDLE/rust/Cargo.lock"
```

Inspect and resolve any links escaping the bundle, and review the entire staged
tree before proceeding. `CACHE` must already be limited to the locked closure;
these copy commands do not filter a full Cargo cache. Record the source and
lockfile hashes from this bundle, not from a different checkout. Only after
confirming that the copied bundle contains no secrets or private data, make it
readable and traversable by the capability-dropped container. This changes only
the reviewed copy, never the host source, toolchain, cache, or home directories:

```sh
chmod -R a+rX "$BUNDLE"
```

`BUNDLE` below is the absolute path to this reviewed bundle. It is mounted
read-only. Do not mount a full home directory. Cache completeness is not
assumed; the offline metadata and build commands below fail if a required
dependency or lockfile update is missing. Supply the missing reviewed cache or
correct the workspace and lockfile before retrying, never fetch during build.

## Build and inspect

Use a fresh container name for every attempt; Docker's fresh writable container
layer provides an initially empty target directory and a private writable Cargo
cache. No host directory is writable in the build. The build executes untrusted
compiler, dependency build-script and procedural-macro code: review the inputs,
use a disposable container and a suitable Docker host isolation boundary. No
Docker socket is mounted; network and Linux capabilities are disabled.

```sh
set -e
BUNDLE=/absolute/path/to/reviewed-bundle
NAME=oms-ubuntu20-build-1
docker create --name "$NAME" --network none --cap-drop ALL \
  --security-opt no-new-privileges --mount "type=bind,src=$BUNDLE,dst=/input,readonly" \
  oms-ubuntu20-builder sh -ec '
    mkdir -p /work/cargo /work/target
    cp -a --no-preserve=ownership /input/cache/registry /input/cache/git /work/cargo/
    chmod -R u+rwX /work/cargo
    export CARGO_HOME=/work/cargo CARGO_TARGET_DIR=/work/target
    export PATH=/input/toolchain/bin:/usr/bin:/bin
    rustc --version --verbose
    cargo --version
    uname -m
    cd /input/rust
    cargo metadata --locked --offline --format-version 1 > /dev/null
    cargo build --locked --offline --release -p oms --bin oms
    cargo build --locked --offline --release -p oms-graph-python --bin oms-graph-python-helper
    for f in /work/target/release/oms /work/target/release/oms-graph-python-helper; do
      readelf -h -l -d -V "$f"
      ldd "$f"
    done
  '
docker start -a "$NAME"
docker inspect "$NAME" --format '{{.State.ExitCode}}'
```

Continue only after a zero exit and review of both ELF headers, interpreter,
dynamic dependencies and required symbol versions. Check `ldd` for missing
libraries; `ldd` output alone does not establish the full closure of libraries
loaded dynamically at runtime. Preserve the output and compare requirements
against the actual deployment image. The explicit binary selection excludes
`oms-graph-python-fixture`; never publish anything else from `target`.

Copy only after success into a new empty staging directory, and verify both
files before handing it off. Use a unique container and staging directory per
attempt. A killed copy can leave a partial file; discard that staging directory
and repeat from a verified successful container. Concurrent attempts must not
share either name or output directory. Copying is not an atomic publication.

```sh
set -e
OUT=/absolute/path/to/new-empty-output
mkdir "$OUT"
docker cp "$NAME":/work/target/release/oms "$OUT/oms"
docker cp "$NAME":/work/target/release/oms-graph-python-helper "$OUT/oms-graph-python-helper"
test -s "$OUT/oms" && test -s "$OUT/oms-graph-python-helper"
chmod a+rX "$OUT" "$OUT/oms" "$OUT/oms-graph-python-helper"
```

The future lifecycle must install `oms` as the sole PATH entry and put the
helper at an absolute private libexec path supplied to the graph Session. This
recipe does not perform that integration or change the live installation.
The permission adjustment applies only to these reviewed, secret-free copied
outputs, allowing the capability-dropped runtime to traverse a private output
directory and execute the files. It does not alter the container artifacts.

## Floor runtime check

Run the selected artifacts in the same reviewed Ubuntu 20.04 base, with the
output directory mounted read-only and no network. Review `ldd` for missing
dependencies and test NSS lookup on the runtime image. The helper has a framed
stdin protocol and no CLI; EOF checks its startup only, not graph behavior.
Follow with the existing graph Session and lifecycle integration tests on the
actual target before declaring deployment readiness.

```sh
set -e
docker run --rm --network none --cap-drop ALL --security-opt no-new-privileges \
  --mount "type=bind,src=$OUT,dst=/artifacts,readonly" "$BASE" sh -ec '
    uname -m
    ldd /artifacts/oms
    ldd /artifacts/oms-graph-python-helper
    getent passwd root
    /artifacts/oms runtime --help
    /artifacts/oms-graph-python-helper < /dev/null
  '
```

Remove the disposable container after retaining the evidence: `docker rm
"$NAME"`. A failed runtime check is a blocker, not proof that copying succeeded.
