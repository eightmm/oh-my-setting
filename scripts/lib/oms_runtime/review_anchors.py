"""Validate finding locations against an exact diff; never resolve a finding."""
from __future__ import annotations

import hashlib
import re

from .workflow_views import DIGEST, ViewError, bounded, identifier, limit_value, rows, text

HUNK = re.compile(r'^@@ -([0-9]{1,10})(?:,([0-9]{1,10}))? \+([0-9]{1,10})(?:,([0-9]{1,10}))? @@(?: .*)?$')
MAX_DIFF_LINES = 100000
MAX_ANCHORS = 100000
ESCAPES = {'a':7,'b':8,'t':9,'n':10,'v':11,'f':12,'r':13,'\\':92,'"':34}


def _path(value, prefix):
    if value == '/dev/null':
        return None
    if value.startswith('"'):
        if not value.endswith('"'):
            raise ViewError('invalid quoted diff path')
        raw = value[1:-1]; data = bytearray(); i = 0
        while i < len(raw):
            ch = raw[i]; i += 1
            if ch != '\\':
                data.extend(ch.encode('utf-8')); continue
            if i == len(raw):
                raise ViewError('invalid quoted diff path escape')
            ch = raw[i]; i += 1
            if ch in ESCAPES:
                data.append(ESCAPES[ch])
            elif ch in '01234567' and i+1 < len(raw) and all(c in '01234567' for c in raw[i:i+2]):
                number = int(ch+raw[i:i+2], 8); i += 2
                if number > 255:
                    raise ViewError('invalid quoted diff path byte')
                data.append(number)
            else:
                raise ViewError('unsupported quoted diff path escape')
        try:
            value = data.decode('utf-8')
        except UnicodeError:
            raise ViewError('non-UTF-8 diff filename is unsupported') from None
    if not value.startswith(prefix+'/'):
        raise ViewError('diff path must carry its canonical side prefix')
    return path_label(value[2:])


def path_label(value):
    if (not isinstance(value, str) or not value or len(value) > 4096 or text(value,4096) != value
            or '\\' in value or value.startswith('/') or any(p in ('','.','..') for p in value.split('/'))):
        raise ViewError('invalid repo-relative review path')
    return value


def parse_diff(raw):
    try:
        content = raw.decode('utf-8')
    except UnicodeError:
        raise ViewError('review diff must be valid UTF-8') from None
    lines = content.split('\n')
    if content.endswith('\n'):
        lines.pop()
    if len(lines) > MAX_DIFF_LINES:
        raise ViewError('review diff exceeds the line budget')
    anchors = {}; old = new = None; header_old = header_new = False
    old_line = new_line = old_left = new_left = 0
    diagnostics = set(); file_count = 0; hunk_seen = False
    def put(path, side, number, kind, value):
        if path is None or number < 1 or number > (1 << 31)-1:
            raise ViewError('invalid diff line position')
        key = (path,side,number)
        if key in anchors:
            raise ViewError('overlapping or duplicate diff hunks')
        if len(anchors) >= MAX_ANCHORS:
            raise ViewError('review diff exceeds the anchor budget')
        anchors[key] = (kind,value)
    for raw_line in lines:
        line = raw_line
        # A CR before patch framing's newline remains part of the source line.
        if old_left or new_left:
            if line == '\\ No newline at end of file':
                continue
            if not line or line[0] not in ' +-':
                raise ViewError('truncated or malformed unified hunk')
            lead, body = line[0], line[1:]
            if lead in ' -':
                if old_left <= 0:raise ViewError('old hunk line count is inconsistent')
                put(old,'old',old_line,'context' if lead==' ' else 'in-diff',body)
                old_left -= 1; old_line += 1
            if lead in ' +':
                if new_left <= 0:raise ViewError('new hunk line count is inconsistent')
                put(new,'new',new_line,'context' if lead==' ' else 'in-diff',body)
                new_left -= 1; new_line += 1
            continue
        if line.startswith(('diff --cc ', 'diff --combined ', '@@@')):
            raise ViewError('combined diffs are unsupported for anchor validation')
        if line.startswith('diff --git '):
            old = new = None; header_old = header_new = False; file_count += 1; hunk_seen = False
        elif line.startswith('--- '):
            if hunk_seen:
                raise ViewError('unexpected file header without a diff boundary')
            old = _path(line[4:],'a'); header_old = True; header_new = False
        elif line.startswith('+++ '):
            if not header_old or header_new or hunk_seen:
                raise ViewError('unexpected new-file diff header')
            new = _path(line[4:],'b'); header_new = True
        elif line.startswith('@@'):
            match = HUNK.fullmatch(line)
            if not match or not header_old or not header_new:
                raise ViewError('invalid unified hunk header')
            a,b,c,d = match.groups()
            old_line,old_left,new_line,new_left = int(a),int(b or 1),int(c),int(d or 1)
            if max(old_line,old_left,new_line,new_left) > (1 << 31)-1:
                raise ViewError('excessive unified hunk range')
            if old_left and old is None or new_left and new is None:
                raise ViewError('hunk content on a nonexistent file side')
            hunk_seen = True
        elif line.startswith('GIT binary patch') or line.startswith('Binary files '):
            diagnostics.add('binary-content-not-inspected')
        elif line.startswith((' ', '+', '-')) and hunk_seen and line != '\\ No newline at end of file':
            raise ViewError('extra lines after a completed hunk')
    if old_left or new_left:
        raise ViewError('diff ended before its hunk counts were satisfied')
    return anchors, sorted(diagnostics), file_count


def validate_anchors(packet, diff_bytes, expected_sha256, *, limit=20):
    limit_value(limit)
    if not isinstance(expected_sha256,str) or not DIGEST.fullmatch(expected_sha256):
        raise ViewError('expected diff digest must be a full SHA-256')
    if not isinstance(diff_bytes, bytes) or len(diff_bytes) > 4 * 1024 * 1024:
        raise ViewError('review diff exceeds the byte budget')
    observed = hashlib.sha256(diff_bytes).hexdigest()
    if observed != expected_sha256:
        raise ViewError('diff does not match the explicitly selected digest')
    if not isinstance(packet,dict) or type(packet.get('schema')) is not int or packet['schema'] != 1:
        raise ViewError('expected schema-1 review findings')
    candidates = rows(packet.get('findings'),256)
    anchors, diagnostics, file_count = parse_diff(diff_bytes)
    result = []; seen=set()
    for row in candidates:
        path = path_label(row.get('path')); side = row.get('side','new'); line=row.get('line'); quote=row.get('quote')
        if (side not in ('old','new') or type(line) is not int or not 1 <= line <= (1<<31)-1
                or not isinstance(quote,str) or not quote or len(quote)>8192 or '\n' in quote):
            raise ViewError('finding requires side, positive line and a bounded single-line quote')
        try:
            quote.encode('utf-8')
        except UnicodeError:
            raise ViewError('finding quote must be valid Unicode') from None
        raw_id = row.get('id')
        finding_id = (identifier(raw_id) if raw_id is not None else
                      'finding-'+hashlib.sha256((path+'\0'+side+'\0'+str(line)+'\0'+quote).encode()).hexdigest()[:24])
        if finding_id in seen:raise ViewError('duplicate finding identifier')
        seen.add(finding_id)
        anchor = anchors.get((path,side,line))
        status = 'outside-observed-hunks' if anchor is None else 'anchor-not-found' if quote not in anchor[1] else anchor[0]
        result.append({'id':finding_id,'path':path,'side':side,'line':line,'anchor_status':status})
    return {'schema':1,'kind':'oms-review-anchor-check','authority':'read-projection-only',
            'diff_sha256':observed,'file_sections':file_count,'diagnostics':diagnostics,
            'findings':bounded(result,limit),'current_tree_verified':False,'gate_verdict_unchanged':True,
            'note':'Locations refer only to the selected diff bytes. Context/outside-hunk/missing anchors do not prove a finding valid or invalid. No finding is resolved or dismissed, and the reviewer/acceptance gate is unchanged. Quotes and source content are not echoed.'}
