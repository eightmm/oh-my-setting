"""Exact internal atomic staging files omitted from worker state snapshots."""

import datetime
import os
import re
import stat


def is_internal_atomic_temp(root, rel):
    """Return true only for a regular file matching a known writer temp."""
    parent, _, name = rel.rpartition("/")
    if parent == "hooks" and re.fullmatch(r"state-hint\.[0-9]{4}-[0-9]{2}-[0-9]{2}", name):
        try:
            date_text = name[len("state-hint."):]
            valid_date = datetime.date.fromisoformat(date_text).isoformat() == date_text
        except ValueError:
            valid_date = False
        if valid_date:
            path = os.path.join(root, *rel.split("/"))
            try:
                info = os.lstat(path)
                reparse_attribute = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
                attributes = getattr(info, "st_file_attributes", 0)
                return (
                    stat.S_ISREG(info.st_mode) and
                    not (attributes & reparse_attribute) and
                    info.st_size == 0
                )
            except OSError:
                return False
    if parent == "hooks/sessions" or parent == "hooks/panel-activity":
        matched = re.fullmatch(r"[A-Za-z0-9._-]+\.json\.[a-z0-9_]{8}\.tmp", name)
    elif parent == "hooks/relay":
        matched = re.fullmatch(r"claude\.json\.[a-z0-9_]{8}\.tmp", name)
    elif parent == "hooks":
        matched = re.fullmatch(r"\.oms-replace\.[a-z0-9_]{8}", name)
    elif parent == "memory":
        matched = re.fullmatch(r"\.oms-replace\.[A-Za-z0-9]{6}", name)
    elif parent == "plan":
        matched = (
            re.fullmatch(r"tmp[a-z0-9_]{8}", name) or
            re.fullmatch(
                r"\.tasks\.[0-9a-f]{64}\.(?:archive|retire-intent)\.json\.tmp",
                name,
            )
        )
    else:
        matched = None

    if matched is None:
        # durable-jsonl atomic mutation and whole-file writers use these two
        # forms. Keep the allowlist on the final targets, not the suffix alone.
        match = re.fullmatch(r"\.(.+)\.tmp\.([0-9]+)(?:\.([0-9a-f]{32}))?", name)
        if match is None:
            return False
        target = (parent + "/" if parent else "") + match.group(1)
        if target in (".gitignore", "artifacts/index.jsonl"):
            matched = True
        elif re.fullmatch(
            r"artifacts/quarantine/artifact-index-[0-9a-f]{64}\.raw", target
        ):
            matched = True
        elif re.fullmatch(
            r"hooks/panel-cache/(?:dashboard|attempts|result-records)\.json", target
        ):
            matched = True
        else:
            panel_id = r"[A-Za-z0-9][A-Za-z0-9._-]{0,159}"
            revision = r"[0-9a-f]{24}"
            matched = (
                re.fullmatch(
                    r"artifacts/panel-results/result-" + panel_id + r"-" +
                    revision + r"\.json", target
                ) is not None or
                re.fullmatch(
                    r"artifacts/panel-results/delivery-" + panel_id + r"-" +
                    revision + r"-[0-9a-f]{16}(?:-pending)?\.json", target
                ) is not None
            )
    if not matched:
        return False

    path = os.path.join(root, *rel.split("/"))
    try:
        info = os.lstat(path)
        reparse_attribute = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
        attributes = getattr(info, "st_file_attributes", 0)
        return (
            stat.S_ISREG(info.st_mode) and
            not (attributes & reparse_attribute)
        )
    except OSError:
        return False
