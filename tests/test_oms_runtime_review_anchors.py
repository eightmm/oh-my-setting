import hashlib
from pathlib import Path
import sys
import unittest

ROOT=Path(__file__).resolve().parents[1];sys.path.insert(0,str(ROOT/'scripts/lib'))
from oms_runtime.review_anchors import validate_anchors
from oms_runtime.workflow_views import ViewError

DIFF=b'diff --git a/test.py b/test.py\n--- a/test.py\n+++ b/test.py\n@@ -1,2 +1,2 @@\n same\n-old\n+new\n'


def check(findings,raw=DIFF,**kwargs):
    return validate_anchors({'schema':1,'findings':findings},raw,hashlib.sha256(raw).hexdigest(),**kwargs)


class ReviewAnchorTests(unittest.TestCase):
    def row(self,**kwargs):return {'path':'test.py','side':'new','line':2,'quote':'new',**kwargs}

    def test_changed_context_old_and_missing_lines(self):
        result=check([self.row(),self.row(line=1,quote='same'),self.row(side='old',quote='old'),self.row(line=8),self.row(quote='wrong')])
        self.assertEqual([r['anchor_status'] for r in result['findings']['items']],
                         ['in-diff','context','in-diff','outside-observed-hunks','anchor-not-found'])
        self.assertTrue(result['gate_verdict_unchanged']);self.assertFalse(result['current_tree_verified'])

    def test_digest_mismatch_refuses_before_results(self):
        with self.assertRaises(ViewError):validate_anchors({'schema':1,'findings':[self.row()]},DIFF,'0'*64)

    def test_add_delete_and_rename(self):
        added=b'diff --git a/new.py b/new.py\n--- /dev/null\n+++ b/new.py\n@@ -0,0 +1 @@\n+added\n'
        self.assertEqual(check([self.row(path='new.py',line=1,quote='added')],added)['findings']['items'][0]['anchor_status'],'in-diff')
        removed=b'diff --git a/old.py b/old.py\n--- a/old.py\n+++ /dev/null\n@@ -1 +0,0 @@\n-deleted\n'
        self.assertEqual(check([self.row(path='old.py',side='old',line=1,quote='deleted')],removed)['findings']['items'][0]['anchor_status'],'in-diff')
        renamed=DIFF.replace(b'+++ b/test.py',b'+++ b/renamed.py')
        self.assertEqual(check([self.row(path='renamed.py')],renamed)['findings']['items'][0]['anchor_status'],'in-diff')

    def test_quoted_utf8_and_space_paths(self):
        path='코드 이름.py'
        encoded=''.join('\\%03o'%b for b in path.encode())
        raw=('diff --git a/x b/x\n--- "a/'+encoded+'"\n+++ "b/'+encoded+'"\n@@ -1 +1 @@\n-old\n+new\n').encode()
        self.assertEqual(check([self.row(path=path,line=1)],raw)['findings']['items'][0]['anchor_status'],'in-diff')

    def test_bad_and_truncated_hunks_rejected(self):
        for raw in (DIFF.replace(b'@@ -1,2 +1,2 @@',b'@@ -1,3 +1,2 @@'),DIFF+b'+extra\n',DIFF.replace(b'--- a/test.py',b'--- a/../outside'),b'diff --cc file\n',DIFF.replace(b' same\n',b'')):
            with self.subTest(raw=raw[:50]),self.assertRaises(ViewError):check([self.row()],raw)

    def test_duplicate_ids_and_invalid_fields_rejected(self):
        with self.assertRaises(ViewError):check([self.row(id='one'),self.row(id='one')])
        for change in ({'line':True},{'line':0},{'path':'/outside'},{'side':'other'},{'quote':'x\ny'},{'quote':''}):
            with self.subTest(change=change),self.assertRaises(ViewError):check([self.row(**change)])

    def test_no_source_quote_or_summary_echo(self):
        result=check([self.row(quote='private-value',summary='private-summary')])
        self.assertNotIn('private-value',str(result));self.assertNotIn('private-summary',str(result))

    def test_limit_and_binary_diagnostic(self):
        raw=DIFF+b'diff --git a/x.bin b/x.bin\nBinary files a/x.bin and b/x.bin differ\n'
        result=check([self.row(id='a'),self.row(id='b')],raw,limit=1)
        self.assertEqual(result['findings']['omitted'],1)
        self.assertIn('binary-content-not-inspected',result['diagnostics'])

    def test_multiple_hunks_and_no_newline_marker(self):
        raw=DIFF+b'\\ No newline at end of file\n@@ -8 +8 @@\n-old8\n+new8\n\\ No newline at end of file\n'
        self.assertEqual(check([self.row(line=8,quote='new8')],raw)['findings']['items'][0]['anchor_status'],'in-diff')
        with self.assertRaises(ViewError):check([self.row()],DIFF+b'@@ -2 +2 @@\n-old\n+new\n')

    def test_lines_starting_header_chars_inside_hunk(self):
        raw=b'diff --git a/test.py b/test.py\n--- a/test.py\n+++ b/test.py\n@@ -1 +1 @@\n---- old\n++++ new\n'
        self.assertEqual(check([self.row(line=1,quote='+++ new')],raw)['findings']['items'][0]['anchor_status'],'in-diff')




class ReviewFramingTests(unittest.TestCase):
    def test_unicode_line_separator_inside_source_is_not_patch_framing(self):
        raw=DIFF.replace(b'+new\n','+new\u2028value\n'.encode())
        self.assertEqual(check([{'path':'test.py','line':2,'quote':'new\u2028value'}],raw)['findings']['items'][0]['anchor_status'],'in-diff')

    def test_excessive_hunk_integer_fails_safely(self):
        raw=DIFF.replace(b'@@ -1,2 +1,2 @@',b'@@ -'+b'1'*5000+b',2 +1,2 @@')
        with self.assertRaises(ViewError):check([{'path':'test.py','line':2,'quote':'new'}],raw)

    def test_unpaired_surrogate_quote_is_refused(self):
        with self.assertRaises(ViewError):check([{'path':'test.py','line':2,'quote':'\ud800'}])

if __name__=='__main__':unittest.main(verbosity=2)
