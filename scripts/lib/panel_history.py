"""Reversible private history retention under the existing allocator and record locks."""

import os
import time

import panel_terminals as terminals


def _move(repo, kind, root, ident, proof, files, archived):
    source_archived = not archived
    with terminals.Store(repo, kind, ident, archived=source_archived) as store, store.lock(timeout=0):
        flag = store.read().get("archived", False)
        if type(flag) is not bool:
            raise ValueError("invalid archive marker")
        # A crash between marker publication and rename leaves either location readable
        # only to this transfer path; completion/ownership proof still precedes any move.
        store.archived = flag
        row = terminals.validate_record(store, ident, {"jobs": "job", "terminals": "shell",
                                                       "workspaces": "workspace"}[kind])
        if not proof(store, row):
            raise ValueError("only positively completed or gone records can move to history")
        if set(os.listdir(store.fd)) != set(files) | {"lock"}:
            raise ValueError("unexpected private record contents")
        for name in (*files, "lock"):
            os.close(store.file(name))
        with terminals.Store(repo, kind, create=True, archived=archived) as destination:
            names = os.listdir(destination.fd)
            limit = terminals.MAX_ARCHIVED if archived else terminals.MAX_ITEMS
            if len(names) > limit + 1 or sum(bool(terminals.ID.fullmatch(name)) for name in names) >= limit:
                raise terminals.CapacityError("History is full; retained records were preserved." if archived else
                                              "Active slots are full; archive a completed item before restoring history.")
            if ident in names:
                raise ValueError("history identity already exists")
            changed = dict(row)
            if archived:
                changed.update(archived=True, archived_at=int(time.time()))
            else:
                changed.pop("archived", None)
                changed.pop("archived_at", None)
            store.write(changed)
            try:
                os.rename(ident, ident, src_dir_fd=root.fd, dst_dir_fd=destination.fd)
            except OSError:
                store.write(row)
                raise
    return {"id": ident, "archived": archived, "history_preserved": True}


def reclaim(repo, kind, root, proof, files, maximum=1):
    """Called with the allocator lock held; running, uncertain and malformed records stay put."""
    moved = []
    deadline = time.monotonic() + 3
    for ident in sorted(name for name in os.listdir(root.fd) if terminals.ID.fullmatch(name)):
        if time.monotonic() >= deadline:
            break
        try:
            moved.append(_move(repo, kind, root, ident, proof, files, True))
        except terminals.CapacityError:
            if moved:
                break
            raise
        except (OSError, ValueError, KeyError, TypeError):
            continue
        if len(moved) >= maximum:
            break
    return moved


def archive(repo, kind, proof, files, ident=None):
    terminals.parent_only()
    try:
        with terminals.Store(repo, kind) as root, root.lock():
            if ident is not None:
                return _move(repo, kind, root, ident, proof, files, True)
            return {"archived": reclaim(repo, kind, root, proof, files, terminals.MAX_ITEMS),
                    "history_preserved": True}
    except FileNotFoundError:
        if ident is not None:
            raise
        return {"archived": [], "history_preserved": True}


def restore(repo, kind, ident, proof, files):
    terminals.parent_only()
    # Archive and restore share the active allocator lock, including across processes.
    with terminals.Store(repo, kind, create=True) as active, active.lock(create=True):
        with terminals.Store(repo, kind, archived=True) as source:
            return _move(repo, kind, source, ident, proof, files, False)


def capacity(repo, kind):
    terminals.parent_only()
    return {"used": len(terminals.identifiers(repo, kind)), "limit": terminals.MAX_ITEMS,
            "archived": len(terminals.identifiers(repo, kind, archived=True)),
            "archive_limit": terminals.MAX_ARCHIVED}
