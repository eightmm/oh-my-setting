# Verified remote artifact collection

`oms runtime ssh-adapter collect` retains its existing single-stream mode.
An optional `--expected-sha256` verifies that stream before atomic destination
replacement. `--resume` adds a local checkpoint cache and requires both a full
SHA-256 and the exact expected byte count from the caller.

```sh
oms runtime ssh-adapter collect \
  --host example --repo . --remote-dir '~/work/project' \
  --remote-path outputs/result.bin --dest artifacts/result.bin \
  --resume --expected-sha256 "$EXPECTED_SHA256" \
  --expected-size 104857600 --max-bytes 134217728
```

Repeat the same request after an interrupted or failed transfer. Successfully
checkpointed chunks are verified locally and skipped. Existing target approval
policy still applies; collection adds no execution or provider authority.
Resumable cancellation returns status 130 during preflight, transfer, assembly,
and the final prepublication check, retaining verified checkpoints.

## Contract and limits

| Input or resource | Contract |
| --- | --- |
| `--resume` | Explicit collection-only flag. Duplicate flags refuse. |
| `--expected-sha256` | Required with resume; exactly 64 lowercase hexadecimal characters. |
| `--expected-size` | Required with resume; an integer from zero through the selected `--max-bytes`. |
| `--max-bytes` | Existing default 64 MiB (67,108,864 bytes); maximum 512 MiB (536,870,912 bytes). Resume does not increase this ceiling. |
| `--chunk-bytes` | Resume only; default 4 MiB, minimum 64 KiB, maximum 8 MiB. Values are bytes, without suffixes. |
| Chunk count | At most 8,192; at the default chunk size, a 512 MiB artifact takes 128 chunk requests. An empty artifact uses one EOF check. |
| Manifest | At most 2 MiB, with bounded records for the exact requested chunk count. |
| Each SSH response | Exact chunk length; an additional byte is enough to refuse overflow. Stderr is bounded to 64 KiB. |
| Each SSH request | 60-second elapsed limit and cancellation checks while draining nonblocking pipes. Local blocking filesystem calls are outside that elapsed limit. |

The adapter uses the existing OpenSSH transport and connection multiplexing.
Remote reads use standard `dd` `if`, `bs`, `skip`, and `count` operands on a
regular file. They need no `rsync`, remote daemon, GNU-only `dd` options, Python,
or new remote deployment. Short reads refuse rather than assuming complete
input blocks. The last request also reads past the expected endpoint to detect
an oversized source. Path checks reject remote traversal, symlink leaves, and
symlink ancestors beneath the requested remote directory.

## Cache identity and recovery

The destination's physical parent contains an owned, mode-0700
`.oms-collect-cache` directory. Each transfer has a private subdirectory named
by the SHA-256 of its identity: host, effective port (including default 22),
remote root, relative remote path, physical destination, expected full digest,
expected size, and chunk size. Different identities never reuse each other's
chunks. Alternate local spellings of the same physical destination and an
explicit port 22 retain the same identity.

`manifest.json` and numbered chunk files must be owned private regular files
with one hard link. Symlink entries, unsafe permissions, malformed manifests,
unknown fields, mismatched identities, invalid chunk indices, recorded missing
chunks, and size or digest changes refuse before fetching missing chunks.
Each chunk is atomically written and synced before its digest is checkpointed
in the manifest. A chunk left without a manifest record by a crash is fetched
again; unrecorded bytes are never silently adopted. Already checkpointed chunks
survive ordinary transport failure or cancellation.

The manifest is only a local checkpoint. It contains no completed/delivered
state and supplies no remote authenticity, immutable snapshot, or permission
claim. A remote file can change between requests. Combining old and new bytes
must still match the caller's exact full size and SHA-256 before publication.
A complete valid cache can satisfy another identical request without contacting
the remote host; this proves the requested bytes, not current remote contents.
Corrupt or mismatched caches are retained for inspection, never automatically
reset. Cache retention has no automatic quota or garbage collector; disk use
includes the retained artifact plus a complete assembly temporary file.

## Publication and concurrency

A nonblocking native lock covers the canonical destination in both ordinary
and resumable collection. A separate lock covers the transfer checkpoint.
Contention returns status 75 before another SSH fetch or destination write.
These are cooperative local-host locks, with the existing native process
identity and stale-owner rules; they do not establish cross-host shared-storage
coordination.

After every required chunk is present, the adapter rereads and verifies the
chunks, assembles a private temporary file beside the destination, and checks
the complete expected size and digest. It syncs the temporary file, rechecks
locks and destination identities, and performs one descriptor-relative atomic
rename. The destination remains absent or retains its prior complete contents
on transfer, cache, size, digest, cancellation, or prepublication failure.
Directory durability is synced where supported; an error after rename reports
that publication occurred but durability is unknown. A crash after publication
and before the response can lose the acknowledgement; the checkpoint is never
treated as a delivery receipt.

Successful resumable JSON includes the existing `bytes`, `sha256`, and
`verified: true` fields, plus `resumable`, `transfer_id`, `chunk_bytes`,
`chunks_reused`, `bytes_reused`, `chunks_fetched`, `bytes_fetched`, and
`cache_dir`. Only the final successful response reports verified collection.
Ordinary collection retains its response shape and optional-digest behavior.

## Verification scope

The canonical `collect_enforces_paths_bounds_and_digest_atomically` test uses
the existing fake-SSH process route to exercise legacy collection and resumable
failure/restart, cached byte reuse, corrupt manifests and chunks, source changes,
wrong size/hash, empty artifacts, cancellation, and concurrent legacy/resumable
writers. `collect_rejects_symlink_ancestors_and_overflow_with_open_stderr`
retains the existing path and stream-bound checks. No live SSH or providers are
needed by these tests. Linux execution does not itself establish macOS runtime
parity; the code uses the existing Unix storage/lock APIs and portable remote
operands.
