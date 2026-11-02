"""离线验证运维摘要白名单；所有身份与错误均为合成数据，不读取生产凭据"""
import contextlib
import importlib.util
import io
import json
import math
from pathlib import Path
import runpy
import sys
import unittest
from unittest.mock import patch

TOOLS = Path(__file__).parents[1] / 'tools'


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, TOOLS / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class AccountSummaryPrivacy(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.verify = load('test_account_verify', 'remote_verify_run.py')
        with patch.dict(sys.modules, {'remote_verify_run': cls.verify}):
            cls.account = load('test_account_summary', 'remote-account-state.py')

    def test_unknown_nested_credentials_and_dynamic_keys_are_not_echoed(self):
        marker = 'fixture-private'
        source = {'credentials': {'password': marker, 'token': marker, 'username': marker},
                  marker: {'account_id': marker}, 'provider_id': marker}
        result = self.account.scrub(source)
        self.assertNotIn(marker, json.dumps(result))
        self.assertNotIn('credentials', json.dumps(result))
        self.assertNotIn('provider_id', result)
        self.assertEqual(result['_unknown_fields'], 2)

    def test_nested_known_container_uses_the_same_field_whitelist(self):
        marker = 'fixture-private'
        result = self.account.scrub({'quota': {
            'remaining': 12, 'enabled': True, 'status': 'active',
            'password': marker, 'session': marker, 'account_id': marker,
            'models': [marker, marker], marker: marker,
        }})
        self.assertEqual(result['_quota']['scalars'], {'remaining': 12, 'enabled': True, 'status': 'active'})
        self.assertEqual(result['_quota']['list_lengths'], {'models': 2})
        self.assertNotIn(marker, json.dumps(result))
        self.assertNotIn('password', json.dumps(result))
        self.assertEqual(result['_quota']['unknown_fields'], 4)

    def test_unknown_short_strings_are_not_treated_as_safe_enums(self):
        for key in ('status', 'state', 'provider', 'provider_id', 'plan', 'authentication_kind'):
            with self.subTest(key=key):
                self.assertIsNone(self.account.safe_scalar('short', key))
        self.assertEqual(self.account.safe_scalar('healthy', 'status'), 'healthy')
        self.assertEqual(self.account.safe_scalar('openai', 'provider'), 'openai')

    def test_scalars_are_field_typed_and_finite(self):
        self.assertIsNone(self.account.safe_scalar(123, 'password'))
        self.assertIsNone(self.account.safe_scalar(123, 'status'))
        self.assertIsNone(self.account.safe_scalar(True, 'remaining'))
        self.assertEqual(self.account.safe_scalar(12, 'remaining'), 12)
        self.assertEqual(self.account.safe_scalar(False, 'enabled'), False)
        for value in (math.inf, -math.inf, math.nan):
            with self.subTest(value=value):
                self.assertIsNone(self.account.safe_scalar(value, 'quota'))

    def test_timestamps_are_parsed_not_passed_through(self):
        self.assertEqual(self.account.safe_scalar('2026-01-01T12:00:00Z', 'expires_at'),
                         '2026-01-01T12:00:00+00:00')
        self.assertIsNone(self.account.safe_scalar('fixture-private', 'expires_at'))

    def test_unexpected_api_shape_does_not_echo_arbitrary_field_names(self):
        marker = 'fixture-private'
        captured = io.StringIO()
        with patch.object(self.verify, 'api', return_value={marker: marker}), contextlib.redirect_stdout(captured):
            code = self.account.main()
        self.assertEqual(code, 1)
        self.assertNotIn(marker, captured.getvalue())
        self.assertEqual(json.loads(captured.getvalue())['top_field_count'], 1)

    def test_account_cli_does_not_print_exception_bodies_or_tracebacks(self):
        marker = 'fixture-private'
        captured = io.StringIO()
        stderr = io.StringIO()
        with patch.dict(sys.modules, {'remote_verify_run': self.verify}), \
             patch.object(self.verify, 'api', side_effect=RuntimeError(marker)), \
             contextlib.redirect_stdout(captured), contextlib.redirect_stderr(stderr):
            with self.assertRaises(SystemExit) as stopped:
                runpy.run_path(str(TOOLS / 'remote-account-state.py'), run_name='__main__')
        self.assertEqual(stopped.exception.code, 1)
        self.assertEqual(json.loads(captured.getvalue())['detail'], 'error_present')
        self.assertNotIn(marker, captured.getvalue())
        self.assertEqual(stderr.getvalue(), '')

    def test_account_main_only_emits_safe_fields(self):
        marker = 'fixture-private'
        payload = {'items': [{'id': marker, 'enabled': True, 'name': marker,
                              'runtime': {'status': 'healthy', 'cookie': marker}}]}
        captured = io.StringIO()
        with patch.object(self.verify, 'api', return_value=payload), contextlib.redirect_stdout(captured):
            code = self.account.main()
        self.assertEqual(code, 0)
        result = json.loads(captured.getvalue())
        self.assertEqual(result['count'], 1)
        self.assertTrue(result['accounts'][0]['enabled'])
        self.assertNotIn(marker, captured.getvalue())


class RunSummaryPrivacy(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.verify = load('test_run_summary', 'remote_verify_run.py')

    def test_errors_of_any_length_are_replaced_with_a_fixed_marker(self):
        marker = 'fixture-private'
        values = ['short', 'password=' + marker, 'username=' + marker,
                  'https://' + 'example.invalid/?unknown=' + marker,
                  '203.0.113.1', '2001:db8::1', 'a' * 200]
        for value in values:
            with self.subTest(length=len(value)):
                self.assertEqual(self.verify.redact(value), 'error_present')
        self.assertEqual(self.verify.redact(''), '')

    def test_submission_reports_counts_not_account_arrays(self):
        marker = 'fixture-private'
        reply = {'started': 2, 'queued': True, 'auth_ids': [marker],
                 'busy': [marker], 'skipped': [marker, marker], 'unchecked': [marker],
                 'credential': marker}
        result = self.verify.submission_summary(reply)
        self.assertEqual(result, {'started': 2, 'queued': True, 'busy_count': 1,
                                  'skipped_count': 2, 'unchecked_count': 1})
        self.assertNotIn(marker, json.dumps(result))

    def test_malformed_submission_cannot_supply_arbitrary_scalar_values(self):
        marker = 'fixture-private'
        for reply in (None, [], {'started': marker, 'busy': marker}, {'started': True}, {'started': -1}):
            with self.subTest(reply_type=type(reply).__name__):
                result = self.verify.submission_summary(reply)
                self.assertEqual(result['started'], 0)
                self.assertNotIn(marker, json.dumps(result))

    def test_record_summaries_only_keep_known_fields_and_magnitudes(self):
        marker = 'fixture-private'
        result = self.verify.summarise({'id': marker, 'error': marker, 'answer': marker,
                                       'input_tokens': 12, 'duration_ms': 500, 'ok': False,
                                       marker: 42, 'password': marker, 'account_id': marker})
        self.assertEqual(result['status'], 'error')
        self.assertEqual(result['error'], 'error_present')
        self.assertEqual(result['numbers'], {'duration_ms': 500, 'input_tokens': 12, 'ok': False})
        self.assertEqual(result['string_chars']['error'], len(marker))
        self.assertEqual(result['unknown_fields'], 3)
        self.assertNotIn(marker, json.dumps(result))
        self.assertNotIn('password', json.dumps(result))
        self.assertNotIn('account_id', json.dumps(result))

    def test_success_and_skipped_statuses_keep_their_meaning(self):
        self.assertEqual(self.verify.summarise({'ok': True})['status'], 'ok')
        self.assertEqual(self.verify.summarise({'skipped': True})['status'], 'skipped')
        self.assertEqual(self.verify.summarise({'ok': 'fixture-private'})['status'], 'unknown')

    def test_main_stdout_does_not_include_submission_ids_or_result_errors(self):
        marker = 'fixture-private'
        before = {'auths': [{'id': marker, 'results': [{'time': '2026-01-01T00:00:00Z'}]}]}
        submitted = {'started': 1, 'busy': [marker], 'auth_ids': [marker], 'skipped': [], 'unchecked': []}
        after = {'auths': [{'id': marker, 'results': [{
            'time': '2026-01-01T00:00:01Z', 'ok': False, 'error': marker, 'input_tokens': 12,
        }]}]}
        captured = io.StringIO()
        with patch.object(sys, 'argv', ['offline-verification']), \
             patch.object(self.verify, 'active_instance', return_value={}), \
             patch.object(self.verify, 'management', side_effect=[before, submitted, after]), \
             patch.object(self.verify.time, 'sleep') as sleep, \
             contextlib.redirect_stdout(captured):
            self.verify.main()
        rows = [json.loads(line) for line in captured.getvalue().splitlines()]
        self.assertEqual(rows[0]['started'], 1)
        self.assertEqual(rows[0]['busy_count'], 1)
        self.assertEqual(rows[-1]['phase'], 'result')
        self.assertNotIn(marker, captured.getvalue())
        sleep.assert_not_called()


if __name__ == '__main__':
    unittest.main()
