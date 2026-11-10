"""Loss-aware metadata digest of an explicit log; never replaces verifier input."""
from __future__ import annotations

from collections import Counter, deque
import hashlib
import re

from .inspection_io import MAX_BYTES, read_segments
from .workflow_views import ViewError, limit_value

MAX_LINE_UNITS = 65536
MAX_CLASSIFY_BYTES = 8192
PATTERNS = (
    ('out-of-memory', re.compile(rb'\b(?:out of memory|oom-kill|killed process)\b', re.I)),
    ('timeout', re.compile(rb'\b(?:timed out|timeout)\b', re.I)),
    ('traceback', re.compile(rb'Traceback \(most recent call last\)', re.I)),
    ('error-word', re.compile(rb'(?:^|[\s:])(?:error|failed|failure|fatal|exception)(?:[\s:!]|$)', re.I)),
    ('non-finite-word', re.compile(rb'\b(?:nan|infinity|inf)\b', re.I)),
)


def digest_output(repo, path, *, exit_code=None, limit=20):
    """Count heuristic signals; disclose every byte/line budget and unknown exit."""
    limit_value(limit)
    if exit_code is not None and (type(exit_code) is not int or not -255 <= exit_code <= 255):
        raise ViewError('exit code must be an integer from -255 through 255')
    read = read_segments(repo, path, max_bytes=MAX_BYTES, allow_partial=True)
    total_signals = scanned = units = long_lines = classify_omitted = 0
    counts, first, last = Counter(), [], deque(maxlen=limit // 2)
    first_capacity = limit - limit // 2
    ranges = []
    lossy = False
    stopped = False
    for offset, content in read['segments']:
        try:
            content.decode('utf-8')
        except UnicodeError:
            lossy = True
        ranges.append({'offset': offset, 'bytes': len(content), 'sha256': hashlib.sha256(content).hexdigest()})
        start = 0
        while start < len(content):
            if units >= MAX_LINE_UNITS:
                stopped = True; break
            end = content.find(b'\n', start)
            end = len(content) if end < 0 else end + 1
            length = end - start
            fragment = (offset > 0 and start == 0) or (end == len(content) and offset+end < read['total_bytes'])
            sample = content[start:min(end, start+MAX_CLASSIFY_BYTES)]
            if length > MAX_CLASSIFY_BYTES:
                long_lines += 1; classify_omitted += length-MAX_CLASSIFY_BYTES
            for kind, pattern in PATTERNS:
                if pattern.search(sample):
                    counts[kind] += 1; total_signals += 1
                    row = {'kind': kind, 'byte_offset': offset+start, 'line_bytes': length,
                           'partial_line': fragment, 'classified_bytes': len(sample)}
                    if len(first) < first_capacity:
                        first.append(row)
                    elif last.maxlen:
                        last.append(row)
            units += 1; scanned += length; start = end
    signals = first + list(last)
    return {'schema': 1, 'kind': 'oms-output-digest', 'authority': 'observation-only',
            'source': {'path': read['path'], 'bytes': read['total_bytes'],
                       'full_sha256': read['sha256'], 'read_ranges': ranges},
            'coverage': {'read_bytes': read['read_bytes'], 'unread_bytes': read['total_bytes']-read['read_bytes'],
                         'scanned_bytes': scanned, 'read_but_unscanned_bytes': read['read_bytes']-scanned,
                         'line_units': units, 'long_lines': long_lines, 'classification_omitted_bytes': classify_omitted,
                         'line_budget_reached': stopped, 'utf8_decode_loss_or_boundary_fragment': lossy,
                         'complete_file_read': read['complete'],
                         'all_read_bytes_classified': not stopped and not classify_omitted},
            'reported_exit_code': exit_code, 'exit_source': 'caller-supplied' if exit_code is not None else 'unknown',
            'signal_counts': dict(counts), 'signals': {'total': total_signals, 'items': signals,
                                                     'omitted': total_signals-len(signals)},
            'note': 'Heuristic word matches, not verified failures or success. No log text or secret values are returned. Original log is unchanged; open it for context. Partial reads have range hashes, never a claimed full-file hash. No command, model, retry or verifier is executed.'}
