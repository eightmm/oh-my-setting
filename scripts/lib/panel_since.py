"""The `since you looked` line of the Attention tab, built from the room's attention read mark.

Display-only. It counts what was recorded after the newest read mark: calls that finished, failures and
requests for the person. It holds counts and words only, and is empty when no read mark exists, the snapshot is
not a completed read, or nothing changed."""
import time

from dashboard_projection import listing, mapping

FAILED_CLASSES = {"failed", "guard_stop", "timed_out", "verify_failed", "verify_timed_out", "provider_error"}


def since_counts(report, ranked, now=None):
    """(finished, failed, needs you, UTC time of the mark), or None when there is nothing to say."""
    marked = getattr(mapping(report).get("attention_read"), "marked_at", None)
    if type(marked) is not int or mapping(report.get("collection")).get("ok") is not True:
        return None
    cutoff = time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(marked))
    later = lambda stamp: len(str(stamp or "")) >= 19 and str(stamp)[:19] > cutoff
    attempts = mapping(report.get("attempts"))
    finished = len({row.get("attempt_id") for key in ("recent", "active_recent") for row in map(mapping, listing(attempts.get(key)))
                    if row.get("state") == "done" and later(row.get("updated_at"))})
    failed = sum(item["cls"] in FAILED_CLASSES and later(item.get("stamp")) for item in ranked)
    needs = sum(item["cls"] == "needs" and later(item.get("stamp")) for item in ranked)
    if not (finished or failed or needs):
        return None
    return finished, failed, needs, cutoff


def since_line(report, ranked, width, now=None):
    """`Since you looked 41m ago: 3 finished · 1 failed · 1 needs you`, shortened to fit `width`; "" when empty."""
    counts = since_counts(report, ranked, now)
    if counts is None:
        return ""
    from panel_view import age_label, clipped, display_width
    finished, failed, needs, cutoff = counts
    ago = age_label(cutoff, now=now)
    forms = (("Since you looked %s ago: " % ago, ("finished", "failed", "needs you")),
             ("Since %s: " % ago, ("done", "fail", "you")))
    for lead, words in forms:
        text = lead + " · ".join("%d %s" % (n, word) for n, word in zip((finished, failed, needs), words) if n)
        if display_width(text) <= width:
            return text
    return clipped(text, width)
