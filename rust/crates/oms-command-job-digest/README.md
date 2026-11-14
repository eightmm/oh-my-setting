# Native job digest

The subprocess queries used by this digest are read-only observations. Each
query gets null stdin and a new process group, so it cannot consume the caller's
input or stop on a terminal read from its background process group.

Output is drained with nonblocking Unix streams in the query loop, without
reader threads. Separate stdout and stderr remain separate, except for `sacct`,
which merges both streams and retains the first 20 lines like the shell digest.
One block per stream per turn keeps cancellation and deadline checks responsive
even when a query continuously writes output. An inherited writer cannot block
a reader-thread join after cancellation.

The direct child stays unreaped until both output streams close. While its
identity remains reserved, cancellation, deadline expiry, or a read failure can
kill the query's process group before reaping the leader. A successful reap
immediately returns; a wait ownership error also returns without signalling the
numeric identity. Neither path can later signal a reused PID or PGID. Deliberate
process-group escapes remain outside group cleanup; closing their inherited
output does not require waiting for EOF. This only terminates local query
processes; it never cancels the observed Slurm job.

The existing observer matrix covers descendants holding output, accounting's
20-line boundary, deadlines, and interruption. Its `stdin-null` case supplies
caller input and checks EOF in `squeue`, `sacct`, and `git` query children.
