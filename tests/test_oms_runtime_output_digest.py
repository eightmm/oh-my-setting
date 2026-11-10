import hashlib
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT=Path(__file__).resolve().parents[1]
sys.path.insert(0,str(ROOT/'scripts/lib'))
from oms_runtime.output_digest import digest_output
from oms_runtime.workflow_views import ViewError


class OutputDigestTests(unittest.TestCase):
    def setUp(self):
        tmp=tempfile.TemporaryDirectory(prefix='oms-log-view-');self.addCleanup(tmp.cleanup)
        self.repo=Path(tmp.name);self.log=self.repo/'run.log'

    def write(self,data):self.log.write_bytes(data)

    def test_signals_exit_and_full_hash(self):
        data=b'hello\nERROR: failed\nCUDA out of memory\nTraceback (most recent call last):\n';self.write(data)
        result=digest_output(self.repo,'run.log',exit_code=1)
        self.assertEqual(result['reported_exit_code'],1)
        self.assertEqual(result['source']['full_sha256'],hashlib.sha256(data).hexdigest())
        self.assertEqual(result['signal_counts']['error-word'],1)
        self.assertEqual(result['signal_counts']['out-of-memory'],1)
        self.assertEqual(self.log.read_bytes(),data)
        self.assertNotIn('success',result)

    def test_no_text_or_secret_values_are_returned(self):
        self.write(b'ERROR: private-fixture-password-is-here\n')
        result=digest_output(self.repo,'run.log')
        self.assertNotIn('private-fixture-password-is-here',str(result))
        self.assertIsNone(result['reported_exit_code'])

    def test_empty_file_is_not_verified_success(self):
        self.write(b'');result=digest_output(self.repo,'run.log')
        self.assertEqual(result['coverage']['scanned_bytes'],0)
        self.assertEqual(result['signal_counts'],{})
        self.assertIsNone(result['reported_exit_code'])
        self.assertNotIn('verified',result)

    def test_partial_ranges_do_not_claim_full_hash(self):
        self.write(b'ERROR first\n'+b'x'*200+b'\nERROR last\n')
        with patch('oms_runtime.output_digest.MAX_BYTES',40):result=digest_output(self.repo,'run.log')
        self.assertIsNone(result['source']['full_sha256'])
        self.assertGreater(result['coverage']['unread_bytes'],0)
        self.assertEqual(result['coverage']['read_bytes'],40)
        self.assertEqual(len(result['source']['read_ranges']),2)

    def test_first_and_last_signals_and_omissions(self):
        self.write(b'ERROR\n'*10);result=digest_output(self.repo,'run.log',limit=3)
        self.assertEqual(result['signals']['total'],10)
        self.assertEqual(result['signals']['omitted'],7)
        self.assertEqual([x['byte_offset'] for x in result['signals']['items']],[0,6,54])

    def test_long_line_classification_loss_is_counted(self):
        self.write(b'x'*9000+b' ERROR\n')
        result=digest_output(self.repo,'run.log')
        self.assertEqual(result['coverage']['long_lines'],1)
        self.assertGreater(result['coverage']['classification_omitted_bytes'],0)
        self.assertFalse(result['coverage']['all_read_bytes_classified'])

    def test_line_budget_is_explicit(self):
        self.write(b'hello\n'*10)
        with patch('oms_runtime.output_digest.MAX_LINE_UNITS',3):result=digest_output(self.repo,'run.log')
        self.assertTrue(result['coverage']['line_budget_reached'])
        self.assertGreater(result['coverage']['read_but_unscanned_bytes'],0)

    def test_unicode_binary_and_control_bytes_are_not_echoed(self):
        self.write('오류 없음\n'.encode()+b'\xff\x1b[31mERROR: private\n')
        result=digest_output(self.repo,'run.log')
        self.assertTrue(result['coverage']['utf8_decode_loss_or_boundary_fragment'])
        self.assertNotIn('\x1b',str(result))
        self.assertNotIn('private',str(result))

    def test_exit_code_validation(self):
        self.write(b'ok')
        for code in (True,256,-256,'1'):
            with self.subTest(code=code),self.assertRaises(ViewError):digest_output(self.repo,'run.log',exit_code=code)
        self.assertEqual(digest_output(self.repo,'run.log',exit_code=-9)['reported_exit_code'],-9)

    def test_no_subprocess_or_implicit_source_discovery(self):
        self.write(b'ERROR\n')
        with patch('subprocess.run',side_effect=AssertionError('subprocess')):
            digest_output(self.repo,'run.log')
        self.assertEqual([p.name for p in self.repo.iterdir()],['run.log'])



if __name__=='__main__':unittest.main(verbosity=2)
