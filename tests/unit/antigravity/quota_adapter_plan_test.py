"""Offline loadCodeAssist plan metadata and two-pool collector fixtures."""
import datetime as dt
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'scripts/account-usage'))
sys.path.insert(0, str(ROOT / 'scripts/antigravity'))
import desktop_helpers as helpers
import desktop_usage as desktop
from auth_platforms import PrivateCredential, VerifiedIdentity, revision
from quota_adapter import SavedProfile, collect_snapshot, existing_collector_dependencies

NOW = dt.datetime(2026, 10, 7, tzinfo=dt.timezone.utc)


def summary():
    return {'groups': [
        {'displayName': label, 'buckets': [
            {'bucketId': prefix + '-5h', 'window': '5h', 'remainingFraction': .4,
             'resetTime': '2026-10-07T05:00:00Z'},
            {'bucketId': prefix + '-weekly', 'window': 'weekly', 'remainingFraction': .8,
             'resetTime': '2026-10-14T00:00:00Z'}]}
        for prefix, label in [('gemini', 'Gemini Models'), ('3p', 'Claude and GPT models')]
    ]}


class AntigravityPlanMetadataTests(unittest.TestCase):
    def test_private_plan_metadata_keeps_ids_and_names_without_tier_secrets(self):
        raw = {'planInfo': {'planType': 'pro', 'private': 'hidden'},
               'paidTier': {'id': 'standard-tier', 'name': 'Google AI Pro', 'availableCredits': []},
               'currentTier': {'id': 'free-tier', 'name': 'Free', 'private': 'hidden'}}
        plan, reported = desktop.antigravity_reported_plan(raw)
        self.assertEqual(plan, 'pro')
        self.assertEqual(reported, {'planType': 'pro',
                                   'paidTier': {'id': 'standard-tier', 'name': 'Google AI Pro'},
                                   'currentTier': {'id': 'free-tier', 'name': 'Free'}})
        self.assertEqual(desktop.antigravity_reported_plan({'currentTier': {'id': 'unknown-tier'}}),
                         ('unknown-tier', {'currentTier': {'id': 'unknown-tier'}}))
        self.assertEqual(desktop.antigravity_reported_plan({'planInfo': {'planType': []},
                         'paidTier': {'id': 'x' * 81, 'name': 'invalid\nname'}}), (None, {}))

    def test_collected_plan_metadata_and_credit_relabel_leave_exactly_two_membership_pools(self):
        raw = json.dumps({'auth_method': 'consumer', 'token': {
            'access_token': 'invented-access', 'refresh_token': 'invented-refresh',
            'token_type': 'Bearer', 'expiry': '2030-01-01T00:00:00Z'}}).encode()
        credential = PrivateCredential(raw, revision(raw))
        identity = VerifiedIdentity('fixture@example.com', '123456789', '2026-10-07T00:00:00Z')
        profile = SavedProfile('fixture', identity.email, identity.identity_key, credential.revision, credential)
        load = {'cloudaicompanionProject': 'invented-project', 'planInfo': {'planType': 'ultra'},
                'paidTier': {'id': 'GOOGLE_AI_ULTRA_20X', 'name': 'Google AI Ultra 20x',
                             'availableCredits': [{'creditAmount': '12.5'}], 'private': 'hidden'}}
        with tempfile.TemporaryDirectory(prefix='aac-plan-fixture-') as root:
            dependencies = existing_collector_dependencies(desktop, helpers, Path(root), now=lambda: NOW)
            with patch.object(helpers, 'request_json', side_effect=[
                {'email': identity.email, 'id': identity.subject, 'verified_email': True}, load, summary()]):
                result = collect_snapshot(profile, dependencies)
        self.assertEqual(result['status'], 'fresh')
        self.assertEqual(result['plan'], 'ultra')
        self.assertEqual(result['reportedPlan']['paidTier'], {'id': 'GOOGLE_AI_ULTRA_20X', 'name': 'Google AI Ultra 20x'})
        self.assertEqual(len(result['pools']), 2)
        self.assertEqual([p['bucketIds'] for p in result['pools']],
                         [['gemini-5h', 'gemini-weekly'], ['3p-5h', '3p-weekly']])
        self.assertEqual(len({w['poolId'] for w in result['windows'] if 'poolId' in w}), 2)
        self.assertTrue(all(p['complete'] and p['eligibility'] == 'reported-quota' for p in result['pools']))
        credits = result['windows'][-1]
        self.assertEqual(credits['key'], 'google-ai-credits')
        self.assertEqual(credits['label'], 'AI credits (overage)')
        self.assertEqual(credits['remaining'], 12.5)
        self.assertIsNone(credits['limit'])
        self.assertNotIn('poolId', credits)
        for private in ('invented-access', 'invented-refresh', 'invented-project', 'hidden'):
            self.assertNotIn(private, json.dumps(result))

    def test_old_samples_keep_new_fields_absent_and_missing_credits_unknown(self):
        self.assertEqual(desktop.antigravity_reported_plan({}), (None, {}))
        self.assertEqual(desktop.normalize_antigravity_credits({}), [])
        self.assertEqual(desktop.normalize_antigravity_credits({'paidTier': {'availableCredits': []}}), [])


if __name__ == '__main__':
    unittest.main()
