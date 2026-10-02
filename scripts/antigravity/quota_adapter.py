"""Identity-bound saved-account quota reads; never replaces the live login."""
from __future__ import annotations
import datetime as dt
import hashlib
import json
import math
import re
from dataclasses import dataclass, field
from typing import Callable
from auth_platforms import AuthError, IdentityMismatch, IdentityUnavailable, NeedsSignIn, PrivateCredential, VerifiedIdentity, parse_credential, refresh_in_memory, verify_identity

@dataclass(frozen=True)
class SavedProfile:
    id: str
    email: str
    identity_key: str = field(repr=False)
    credential_revision: str = field(repr=False)
    credential: PrivateCredential = field(repr=False)

@dataclass
class QuotaDependencies:
    userinfo: Callable[[str], dict]
    refresh: Callable[[str], dict]
    quota: Callable[[str, str], dict]
    now: Callable[[], dt.datetime] = lambda: dt.datetime.now(dt.timezone.utc)


def collect_snapshot(profile: SavedProfile, deps: QuotaDependencies) -> dict:
    """Private internal DTO; callers discard identity/revision in public data.

    The source revision binds the *saved* bundle, even when its access token is
    temporarily renewed in memory. No opaque credentials or projects leave this
    function. Tokens and API responses never appear in failures.
    """
    base = {'profileId': profile.id, 'identityKey': profile.identity_key,
            'credentialRevision': profile.credential_revision,
            'status': 'unavailable', 'email': profile.email, 'plan': None,
            'fetchedAt': None, 'sampledAt': None, 'windows': [],
            'source': 'native-consumer', 'identityVerified': False, 'identityValidation': 'unavailable', 'pools': []}
    try:
        if profile.credential.revision != profile.credential_revision:
            raise IdentityMismatch('The saved Antigravity account changed before its usage query.')
        current = profile.credential
        now = deps.now()
        if now.tzinfo is None:
            raise AuthError('A timezone-aware observation time is required.')
        expiry = dt.datetime.fromisoformat(parse_credential(current.raw)['token']['expiry'].replace('Z', '+00:00'))
        if expiry <= now + dt.timedelta(seconds=30):
            current = refresh_in_memory(current, deps.refresh, now)
        identity = verify_identity(current, deps.userinfo)
        if identity.identity_key != profile.identity_key or identity.email != profile.email.lower():
            raise IdentityMismatch('The saved Antigravity login belongs to another account.')
        base.update(identityVerified=True, identityValidation='verified')
        data = deps.quota(parse_credential(current.raw)['token']['access_token'], identity.email)
        if not isinstance(data, dict) or not isinstance(data.get('windows'), list):
            raise AuthError('Antigravity did not return a usable quota sample.')
        if data.get('email', identity.email).lower() != identity.email:
            raise IdentityMismatch('Antigravity quota identity did not match the saved account.')
        # The existing normalizer's bounded DTO is the only output. Never
        # derive model pool IDs or entitlement from display names/percentages.
        windows = []
        allowed = {'key','label','usedPercent','remainingPercent','resetAt','windowMinutes','used','limit','unit','kind','remaining','expiresAt','unlimited','enabled','poolId','poolIdSource','poolLabel','modelIds'}
        for window in data['windows'][:64]:
            if isinstance(window, dict): windows.append({k:v for k,v in window.items() if k in allowed})
        stamp = deps.now().astimezone(dt.timezone.utc).isoformat(timespec='seconds').replace('+00:00', 'Z')
        base.update(status='fresh', email=identity.email, plan=data.get('plan'),
                    fetchedAt=stamp, sampledAt=stamp, windows=windows, pools=data.get('pools', []))
    except IdentityMismatch:
        base.update(status='needs_sign_in', identityVerified=False, identityValidation='mismatch')
    except NeedsSignIn:
        base.update(status='needs_sign_in', identityVerified=False, identityValidation='needs_sign_in')
    except IdentityUnavailable:
        base.update(status='unavailable', identityVerified=False, identityValidation='unavailable')
    except AuthError:
        base['status'] = 'unavailable'
    except Exception as error:
        status = getattr(error, 'status', None)
        base['status'] = 'rate_limited' if status == 'rate_limited' else 'unavailable' if status in ('needs_sign_in', 'unavailable') else 'error'
        # Deliberately no exception/upstream text or guessed retry time.
    return base


def existing_collector_dependencies(desktop, helpers, home, now=None) -> QuotaDependencies:
    """Reuse the installed collector's private metadata and HTTPS transport.

    Both modules are explicit trusted package imports. No file/keyring write,
    new login, inferred model mapping or app OAuth constant is introduced.
    """
    def userinfo(access):
        return helpers.request_json('https://www.googleapis.com/oauth2/v2/userinfo', {'Authorization': 'Bearer ' + access})
    def refresh(refresh_token):
        client_id, client_secret = desktop.antigravity_oauth_client(home)
        return helpers.request_json('https://oauth2.googleapis.com/token', body={
            'grant_type': 'refresh_token', 'refresh_token': refresh_token,
            'client_id': client_id, 'client_secret': client_secret,
        }, form=True)
    def quota(access, email):
        headers = {'Authorization': 'Bearer ' + access, 'User-Agent': 'antigravity/1.0.0'}
        load = helpers.request_json(desktop.AGY_BASE + 'loadCodeAssist', headers,
            {'metadata': {'ideType': 'ANTIGRAVITY', 'platform': 'PLATFORM_UNSPECIFIED', 'pluginType': 'GEMINI'}})
        project = load.get('cloudaicompanionProject')
        if isinstance(project, dict): project = project.get('id') or project.get('projectId')
        project = helpers.safe_text(project)
        if project is None:
            raise AuthError('Antigravity did not return a usable quota project.')
        tier = load.get('paidTier') or load.get('currentTier')
        info = load.get('planInfo')
        plan = (helpers.safe_text(info.get('planType')) if isinstance(info, dict) else None) or (
            helpers.safe_text(tier.get('name')) or helpers.safe_text(tier.get('id')) if isinstance(tier, dict) else None)
        summary = helpers.request_json(desktop.AGY_BASE + 'retrieveUserQuotaSummary', headers, {'project': project})
        windows = desktop.normalize_antigravity(summary)
        pools = structural_pools(desktop.agy_groups(summary), windows)
        windows += desktop.normalize_antigravity_credits(load)
        return {'email': email, 'plan': plan, 'windows': windows, 'pools': pools}
    return QuotaDependencies(userinfo, refresh, quota, now or (lambda: dt.datetime.now(dt.timezone.utc)))


def structural_pools(groups: list, windows: list) -> list:
    """Preserve actual group membership without guessing from model labels.

    IDs derive from sorted exact provider bucket IDs. Public display includes
    the derivation source; this never claims a provider-supplied pool ID or
    entitlement flag. Missing/duplicate buckets or readings defer automation.
    """
    pools, counts = [], {}
    for group in groups:
        if not isinstance(group, dict) or not isinstance(group.get('buckets'), list): continue
        for bucket in group['buckets']:
            if isinstance(bucket, dict) and isinstance(bucket.get('bucketId'), str):
                counts[bucket['bucketId']] = counts.get(bucket['bucketId'], 0) + 1
    lookup = {w.get('key'): w for w in windows if isinstance(w, dict)}
    for group in groups[:32]:
        if not isinstance(group, dict) or not isinstance(group.get('buckets'), list): continue
        buckets = group['buckets']
        ids = [b.get('bucketId') for b in buckets if isinstance(b, dict)]
        if (len(ids) != len(buckets) or not ids or len(ids) > 32
                or any(not isinstance(k, str) or not re.fullmatch(r'[A-Za-z0-9_.:-]{1,160}', k) for k in ids)):
            continue
        digest = hashlib.sha256(json.dumps(sorted(ids), separators=(',', ':'), ensure_ascii=False).encode()).hexdigest()
        pool_id = 'bucket-group:' + digest
        selected = [lookup[k] for k in ids if k in lookup]
        # Display normalization omits malformed optional flags. Validate the
        # original provider values before any private automation eligibility
        # decision so a present unknown value cannot become an absent flag.
        flags_valid = all(key not in bucket or type(bucket[key]) is bool
            for bucket in buckets for key in ('disabled', 'unlimited'))
        complete = flags_valid and len(selected) == len(ids) and all(counts[k] == 1 for k in ids)
        complete = complete and all(w.get('kind') == 'rate_limit' and type(w.get('remainingPercent')) in (int, float)
            and math.isfinite(w['remainingPercent']) and 0 <= w['remainingPercent'] <= 100
            and isinstance(w.get('resetAt'), str) for w in selected)
        eligibility = 'unverified' if not flags_valid or any(b.get('disabled') is True for b in buckets) else 'reported-quota'
        for window in selected:
            if counts[window['key']] == 1:
                window.update(poolId=pool_id, poolIdSource='provider-bucket-membership')
        actual_label = group.get('displayName')
        if (isinstance(actual_label, str) and 0 < len(actual_label) <= 160
                and not re.search(r'[\x00-\x1f\x7f]', actual_label)
                and not re.search(r'(?i)(bearer\s|dca:|sk-[A-Za-z0-9]|eyJ[A-Za-z0-9_-]{8})', actual_label)):
            for window in selected: window['poolLabel'] = actual_label
        # modelIds are preserved only when the provider explicitly supplies
        # them; unknown model membership is left absent.
        actual_models = group.get('modelIds')
        if isinstance(actual_models, list) and actual_models and all(isinstance(v, str) and re.fullmatch(r'[A-Za-z0-9_.:/-]{1,160}', v) for v in actual_models):
            for window in selected: window['modelIds'] = list(actual_models)
        pools.append({'id': pool_id, 'idSource': 'provider-bucket-membership',
            'eligibility': eligibility, 'complete': complete, 'bucketIds': sorted(ids), 'windows': selected})
    return pools
