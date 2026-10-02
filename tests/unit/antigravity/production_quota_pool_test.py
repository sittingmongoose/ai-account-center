"""Raw synthetic provider flags through the retained normalizer/private pool gate."""
from copy import deepcopy
from pathlib import Path
import sys
import unittest

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'scripts/account-usage'))
sys.path.insert(0, str(ROOT / 'scripts/antigravity'))
import desktop_usage
from quota_adapter import structural_pools


def sample():
    return {'groups': [
        {'displayName': 'Reported Gemini pool', 'buckets': [
            {'bucketId': 'gemini-5h', 'window': '5h', 'remainingFraction': 0.4,
             'resetTime': '2026-10-02T00:00:00Z'},
            {'bucketId': 'gemini-weekly', 'window': 'weekly', 'remainingFraction': 0.8,
             'resetTime': '2026-10-08T00:00:00Z'}]},
        {'displayName': 'Reported third-party pool', 'buckets': [
            {'bucketId': 'third-party-5h', 'window': '5h', 'remainingFraction': 0.7,
             'resetTime': '2026-10-02T00:00:00Z'},
            {'bucketId': 'third-party-weekly', 'window': 'weekly', 'remainingFraction': 0.9,
             'resetTime': '2026-10-08T00:00:00Z'}]},
    ]}


def collect(summary):
    windows = desktop_usage.normalize_antigravity(summary)
    before = deepcopy(windows)
    pools = structural_pools(desktop_usage.agy_groups(summary), windows)
    return before, windows, pools


class RawQuotaFlagTests(unittest.TestCase):
    def test_absent_flags_preserve_both_actual_groups_and_constraints(self):
        _, windows, pools = collect(sample())
        self.assertEqual(len(windows), 4)
        self.assertEqual(len(pools), 2)
        self.assertTrue(all(p['complete'] for p in pools))
        self.assertTrue(all(p['eligibility'] == 'reported-quota' for p in pools))
        self.assertNotEqual(pools[0]['id'], pools[1]['id'])
        for pool in pools:
            self.assertEqual({w['windowMinutes'] for w in pool['windows']}, {300, 10080})
            self.assertTrue(all(w['resetAt'] for w in pool['windows']))

    def test_exact_false_flags_remain_valid_not_coerced(self):
        data = sample()
        data['groups'][0]['buckets'][0].update(disabled=False, unlimited=False)
        _, windows, pools = collect(data)
        self.assertTrue(pools[0]['complete'])
        self.assertEqual(pools[0]['eligibility'], 'reported-quota')
        self.assertIs(windows[0]['enabled'], True)
        self.assertIs(windows[0]['unlimited'], False)

    def test_disabled_true_remains_unverified_and_display_disabled(self):
        data = sample()
        data['groups'][0]['buckets'][0]['disabled'] = True
        _, windows, pools = collect(data)
        self.assertIs(windows[0]['enabled'], False)
        self.assertEqual(pools[0]['eligibility'], 'unverified')

    def test_unlimited_true_is_preserved_for_existing_policy_rejection(self):
        data = sample()
        data['groups'][0]['buckets'][0]['unlimited'] = True
        _, windows, pools = collect(data)
        self.assertIs(windows[0]['unlimited'], True)
        self.assertIs(pools[0]['windows'][0]['unlimited'], True)

    def test_malformed_raw_flags_cannot_be_laundered_into_eligible_pools(self):
        for name in ('disabled', 'unlimited'):
            for bad in ('true', 'false', 0, 1, 0.0, 1.0, None, {}, []):
                with self.subTest(flag=name, value=repr(bad)):
                    data = sample()
                    data['groups'][0]['buckets'][0][name] = bad
                    before, windows, pools = collect(data)
                    # Existing display omission is retained; private raw input
                    # must separately retain the fact that this flag was present.
                    self.assertNotIn('enabled' if name == 'disabled' else 'unlimited', before[0])
                    self.assertEqual(len(windows), 4)
                    self.assertFalse(pools[0]['complete'])
                    self.assertEqual(pools[0]['eligibility'], 'unverified')
                    self.assertTrue(pools[1]['complete'])
                    self.assertEqual(pools[1]['eligibility'], 'reported-quota')
                    self.assertEqual(windows[0]['remainingPercent'], 40)
                    self.assertEqual(windows[0]['resetAt'], '2026-10-02T00:00:00Z')

    def test_one_malformed_flag_vetoes_valid_companion_flag(self):
        data = sample()
        data['groups'][0]['buckets'][0].update(disabled=False, unlimited='false')
        _, windows, pools = collect(data)
        self.assertIs(windows[0]['enabled'], True)
        self.assertFalse(pools[0]['complete'])
        self.assertEqual(pools[0]['eligibility'], 'unverified')


if __name__ == '__main__':
    unittest.main()
