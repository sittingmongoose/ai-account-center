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
                {'email': identity.email, 'id': identity.subject, 'verified_email': True}, load, summary(),
                helpers.UsageError('unavailable', 'Per-model quota is unavailable.')]):
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

    def test_collector_derives_gemini_window_when_summary_omits_the_group(self):
        raw = json.dumps({'auth_method': 'consumer', 'token': {
            'access_token': 'invented-access', 'refresh_token': 'invented-refresh',
            'token_type': 'Bearer', 'expiry': '2030-01-01T00:00:00Z'}}).encode()
        credential = PrivateCredential(raw, revision(raw))
        identity = VerifiedIdentity('fixture@example.com', '123456789', '2026-10-07T00:00:00Z')
        profile = SavedProfile('fixture', identity.email, identity.identity_key, credential.revision, credential)
        load = {'cloudaicompanionProject': 'invented-project', 'planInfo': {'planType': 'pro'}}
        # The summary shape Google has served since 2026-10-22: no Gemini group.
        without_gemini = {'groups': [{'displayName': 'Claude and GPT models', 'buckets': [
            {'bucketId': '3p-5h', 'window': '5h', 'remainingFraction': .9, 'resetTime': '2026-10-07T05:00:06Z'},
            {'bucketId': '3p-weekly', 'window': 'weekly', 'remainingFraction': .9, 'resetTime': '2026-10-14T00:00:06Z'}]}]}
        per_model = {'buckets': [
            {'modelId': 'gemini-3.8-flash-tiered', 'tokenType': 'WTUS', 'remainingFraction': .55, 'resetTime': '2026-10-07T05:00:00Z'},
            {'modelId': 'gemini-3.1-pro', 'tokenType': 'WTUS', 'remainingFraction': .4, 'resetTime': '2026-10-07T05:00:00Z'}]}
        with tempfile.TemporaryDirectory(prefix='aac-gemini-fixture-') as root:
            dependencies = existing_collector_dependencies(desktop, helpers, Path(root), now=lambda: NOW)
            with patch.object(helpers, 'request_json', side_effect=[
                    {'email': identity.email, 'id': identity.subject, 'verified_email': True},
                    load, without_gemini, per_model]):
                result = collect_snapshot(profile, dependencies)
        self.assertEqual(result['status'], 'fresh')
        derived = [window for window in result['windows'] if window['key'].startswith('gemini')]
        self.assertEqual(len(derived), 1)
        self.assertEqual(derived[0]['key'], 'gemini-5h')
        self.assertEqual(derived[0]['label'], 'Gemini Models · 5-hour')
        self.assertEqual(derived[0]['remainingPercent'], 40)
        self.assertEqual(derived[0]['modelIds'], ['gemini-3.1-pro', 'gemini-3.8-flash-tiered'])
        # A derived window carries no provider bucket membership and no pool.
        self.assertNotIn('poolId', derived[0])
        self.assertNotIn('private', json.dumps(result))

    def _gemini_fixture(self):
        raw = json.dumps({'auth_method': 'consumer', 'token': {
            'access_token': 'invented-access', 'refresh_token': 'invented-refresh',
            'token_type': 'Bearer', 'expiry': '2030-01-01T00:00:00Z'}}).encode()
        credential = PrivateCredential(raw, revision(raw))
        identity = VerifiedIdentity('fixture@example.com', '123456789', '2026-10-07T00:00:00Z')
        profile = SavedProfile('fixture', identity.email, identity.identity_key, credential.revision, credential)
        load = {'cloudaicompanionProject': 'invented-project', 'planInfo': {'planType': 'pro'}}
        without_gemini = {'groups': [{'displayName': 'Claude and GPT models', 'buckets': [
            {'bucketId': '3p-5h', 'window': '5h', 'remainingFraction': .9, 'resetTime': '2026-10-07T05:00:06Z'}]}]}
        userinfo = {'email': identity.email, 'id': identity.subject, 'verified_email': True}
        return profile, userinfo, load, without_gemini

    def test_collector_makes_no_per_model_read_when_summary_has_gemini(self):
        profile, userinfo, load, _ = self._gemini_fixture()
        restored = {'groups': [
            {'displayName': 'Gemini Models', 'buckets': [
                {'bucketId': 'gemini-5h', 'window': '5h', 'remainingFraction': .4, 'resetTime': '2026-10-07T05:00:00Z'}]},
            {'displayName': 'Claude and GPT models', 'buckets': [
                {'bucketId': '3p-5h', 'window': '5h', 'remainingFraction': .9, 'resetTime': '2026-10-07T05:00:06Z'}]}]}
        with tempfile.TemporaryDirectory(prefix='aac-gemini-restored-') as root:
            dependencies = existing_collector_dependencies(desktop, helpers, Path(root), now=lambda: NOW)
            with patch.object(helpers, 'request_json', side_effect=[userinfo, load, restored]) as request:
                result = collect_snapshot(profile, dependencies)
        self.assertEqual(result['status'], 'fresh')
        self.assertEqual([window['key'] for window in result['windows']], ['gemini-5h', '3p-5h'])
        self.assertEqual(request.call_count, 3)
        self.assertNotIn('retrieveUserQuota', [call.args[0] for call in request.call_args_list])
        self.assertNotIn('retryAfterSeconds', result)

    def test_collector_publishes_cooldown_when_the_optional_read_is_throttled(self):
        profile, userinfo, load, without_gemini = self._gemini_fixture()
        with tempfile.TemporaryDirectory(prefix='aac-gemini-throttle-') as root:
            dependencies = existing_collector_dependencies(desktop, helpers, Path(root), now=lambda: NOW)
            with patch.object(helpers, 'request_json', side_effect=[
                    userinfo, load, without_gemini,
                    helpers.UsageError('rate_limited', 'Throttled.', retry_after=1800)]):
                result = collect_snapshot(profile, dependencies)
        self.assertEqual(result['status'], 'fresh')
        self.assertEqual([window['key'] for window in result['windows']], ['3p-5h'])
        self.assertEqual(result['retryAfterSeconds'], 1800)

    def test_collector_skips_the_optional_read_once_the_time_budget_is_spent(self):
        profile, userinfo, load, without_gemini = self._gemini_fixture()
        clock = [NOW]
        per_model = {'buckets': [
            {'modelId': 'gemini-3.8-flash-tiered', 'remainingFraction': .55, 'resetTime': '2026-10-07T05:00:00Z'}]}
        requested = []

        def slow_request(url, *args, **kwargs):
            requested.append(url)
            # Each HTTPS call consumes 6s of the worker's budget, from the
            # dependencies' construction (worker start) onward.
            clock[0] += dt.timedelta(seconds=6)
            for suffix, payload in (('loadCodeAssist', load),
                                    ('retrieveUserQuotaSummary', without_gemini),
                                    ('userinfo', userinfo),
                                    ('token', {'access_token': 'renewed-fixture', 'expires_in': 3600}),
                                    ('retrieveUserQuota', per_model)):
                if url.endswith(suffix):
                    return payload
            raise AssertionError('unexpected url ' + url)

        with tempfile.TemporaryDirectory(prefix='aac-gemini-budget-') as root:
            dependencies = existing_collector_dependencies(desktop, helpers, Path(root), now=lambda: clock[0])
            with patch.object(helpers, 'request_json', side_effect=slow_request):
                result = collect_snapshot(profile, dependencies)
        # userinfo + load + summary consume 18s of the 25s worker budget; the
        # optional endpoint is never contacted and the summary still publishes.
        self.assertEqual(result['status'], 'fresh')
        self.assertEqual([window['key'] for window in result['windows']], ['3p-5h'])
        self.assertFalse([url for url in requested if url.endswith('retrieveUserQuota')])
        self.assertNotIn('retryAfterSeconds', result)

    def test_collector_budget_includes_token_refresh_and_identity(self):
        raw = json.dumps({'auth_method': 'consumer', 'token': {
            'access_token': 'invented-access', 'refresh_token': 'invented-refresh',
            'token_type': 'Bearer', 'expiry': '2026-10-06T00:00:00Z'}}).encode()
        credential = PrivateCredential(raw, revision(raw))
        identity = VerifiedIdentity('fixture@example.com', '123456789', '2026-10-07T00:00:00Z')
        profile = SavedProfile('fixture', identity.email, identity.identity_key, credential.revision, credential)
        _p, userinfo, load, without_gemini = self._gemini_fixture()
        clock = [NOW]
        requested = []

        def slow_request(url, *args, **kwargs):
            requested.append(url)
            clock[0] += dt.timedelta(seconds=6)
            for suffix, payload in (('token', {'access_token': 'renewed-fixture', 'expires_in': 3600}),
                                    ('userinfo', userinfo),
                                    ('loadCodeAssist', load),
                                    ('retrieveUserQuotaSummary', without_gemini),
                                    ('retrieveUserQuota', AssertionError('optional call forbidden'))):
                if url.endswith(suffix):
                    if isinstance(payload, Exception):
                        raise payload
                    return payload
            raise AssertionError('unexpected url ' + url)

        with tempfile.TemporaryDirectory(prefix='aac-gemini-refresh-budget-') as root:
            dependencies = existing_collector_dependencies(desktop, helpers, Path(root), now=lambda: clock[0])
            with patch.object(desktop, 'antigravity_oauth_client',
                              return_value=('fixture-client.apps.googleusercontent.com', 'synthetic-app-value')), \
                    patch.object(helpers, 'request_json', side_effect=slow_request):
                result = collect_snapshot(profile, dependencies)
        # The expired-token refresh and identity verification consume the
        # budget before the summary completes, so the optional read cannot
        # push the worker past its deadline and lose the usable summary.
        self.assertEqual(result['status'], 'fresh')
        self.assertEqual([window['key'] for window in result['windows']], ['3p-5h'])
        self.assertFalse([url for url in requested if url.endswith('retrieveUserQuota')])


if __name__ == '__main__':
    unittest.main()
