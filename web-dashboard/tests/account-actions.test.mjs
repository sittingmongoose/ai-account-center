// What each control sends and what each answer means (public/account-actions.mjs).
import test from 'node:test';
import assert from 'node:assert/strict';
import { requests, errorText, jobErrorText, unavailableText, profileNameProblem, claudeIdProblem, keyProblem, suggestName, passwordProblem, passwordChangedToast, revokeAllToast, copyText } from '../public/account-actions.mjs';

const err = (status, code, extra = {}) => ({ status, payload: { error: 'server sentence', code, ...extra } });

test('every request goes to the route and body the CLIENT API SHEET names', () => {
  assert.deepEqual(requests.registry(), { method: 'GET', path: '/api/accounts/registry' });
  assert.deepEqual(requests.visibility(['kimi-code'], ['codex:a']), { method: 'PUT', path: '/api/accounts/visibility', body: { hiddenProviders: ['kimi-code'], hiddenAccountIds: ['codex:a'] } });
  assert.deepEqual(requests.trayVisibility(['zai']).body, { trayHiddenProviders: ['zai'] });
  assert.deepEqual(requests.addCodex('codex-4').body, { provider: 'codex', profileName: 'codex-4' });
  assert.deepEqual(requests.addClaude('party', '').body, { provider: 'claude', profileId: 'party' });
  assert.deepEqual(requests.addClaude('party', 'Party').body, { provider: 'claude', profileId: 'party', label: 'Party' });
  assert.deepEqual(requests.addKey('zai', 'k-12345678', '').body, { provider: 'zai', key: 'k-12345678' });
  assert.deepEqual(requests.addKey('zai', 'k-12345678', 'Work').body, { provider: 'zai', key: 'k-12345678', label: 'Work' });
  assert.deepEqual(requests.addSession('cursor').body, { provider: 'cursor' });
  assert.deepEqual(requests.signInAgain('codex:one'), { method: 'POST', path: '/api/accounts/codex%3Aone/signin-again', body: {} });
  assert.deepEqual(requests.replaceKey('zai:acct:9f2c41d0', 'k'), { method: 'PUT', path: '/api/accounts/zai%3Aacct%3A9f2c41d0/key', body: { key: 'k' } });
  assert.deepEqual(requests.removeAsk('claude:party').body, {});
  assert.deepEqual(requests.removeCommit('claude:party', 't').body, { confirmationToken: 't' });
  assert.deepEqual(requests.openApp('cursor:usage', 'mac'), { method: 'POST', path: '/api/accounts/cursor%3Ausage/open', body: { platform: 'mac' } });
  assert.deepEqual(requests.recheck('qwen:usage').path, '/api/accounts/qwen%3Ausage/recheck');
  assert.deepEqual(requests.restoreAsk('tr_1'), { method: 'POST', path: '/api/accounts/trash/tr_1/restore', body: {} });
  assert.deepEqual(requests.restoreCommit('tr_1', 't').body, { confirmationToken: 't' });
  assert.deepEqual(requests.job('job_1'), { method: 'GET', path: '/api/accounts/signin-jobs/job_1' });
  assert.deepEqual(requests.cancelJob('job_1'), { method: 'POST', path: '/api/accounts/signin-jobs/job_1/cancel', body: {} });
  assert.deepEqual(requests.submitCode('job_1', 'c').body, { code: 'c' });
  assert.deepEqual(requests.session(), { method: 'GET', path: '/api/auth/session' });
  assert.deepEqual(requests.devices(), { method: 'GET', path: '/api/auth/devices' });
  assert.deepEqual(requests.network(), { method: 'GET', path: '/api/auth/network' });
  assert.deepEqual(requests.setNetwork(false), { method: 'PUT', path: '/api/auth/network', body: { trustLocalNetwork: false } });
  assert.deepEqual(requests.password('a', 'b', true).body, { currentPassword: 'a', newPassword: 'b', signOutOtherBrowsers: true });
  assert.deepEqual(requests.password('a', 'b', false).body.signOutOtherBrowsers, false);
  assert.deepEqual(requests.revokeOthers(), { method: 'POST', path: '/api/auth/sessions/revoke-others', body: {} });
  assert.deepEqual(requests.revokeDevice('dev_1'), { method: 'DELETE', path: '/api/auth/devices/dev_1', body: {} });
  assert.deepEqual(requests.revokeAll(), { method: 'POST', path: '/api/auth/devices/revoke-all', body: { signOutOtherBrowsers: true } });
  assert.deepEqual(requests.logout(), { method: 'POST', path: '/api/auth/logout', body: {} });
});

test('every server error code has its own calm words, never the server sentence', () => {
  const codes = ['auth_required', 'session_revoked', 'device_scope', 'origin_required', 'json_required', 'invalid_json', 'unexpected_query', 'body_too_large', 'invalid_body', 'not_found', 'token_in_query', 'internal_error',
    'auth_not_configured', 'managed_by_env', 'weak_password', 'same_password', 'wrong_password', 'invalid_credentials', 'rate_limited', 'invalid_hash', 'secure_transport_required', 'loopback_required', 'write_failed',
    'unknown_device', 'auth_store_unavailable', 'too_many_devices', 'already_configured', 'setup_code_required', 'setup_code_invalid', 'invalid_username', 'visibility_unavailable', 'visibility_write_failed',
    'registry_unavailable', 'invalid_account', 'unknown_account', 'not_implemented', 'not_configured', 'not_removable', 'single_account_provider', 'too_many_accounts', 'id_in_use', 'duplicate_key', 'key_rejected',
    'key_store_unavailable', 'not_aac_owned', 'use_replace_key', 'account_active', 'account_default', 'account_protected', 'last_account', 'activation_running', 'signin_running', 'job_running', 'too_many_jobs',
    'app_running', 'app_state_unknown', 'trash_cross_volume', 'remove_failed', 'restore_failed', 'unknown_trash', 'confirmation_stale', 'host_unreachable', 'tool_missing', 'unknown_job', 'code_not_expected',
    'preflight_failed', 'isolation_unproven', 'extension_update_required', 'session_rotation_failed'];
  const seen = new Set();
  for (const code of codes) {
    const t = errorText(err(409, code), { provider: 'codex' });
    assert.equal(t.code, code);
    assert.ok(t.title && t.title !== 'That did not work', code);
    assert.doesNotMatch(`${t.title} ${t.body}`, /server sentence|HTTPS|tunnel/, code);
    seen.add(`${t.title}|${t.body}`);
  }
  // only the four request-shape refusals share their words ("Reload the page and try again")
  assert.equal(seen.size, codes.length - 3, 'the words differ per code');
  // the details each answer carries
  assert.equal(errorText(err(401, 'wrong_password', { triesLeft: 2 })).body, '2 tries left before a 15-minute pause.');
  assert.equal(errorText(err(401, 'invalid_credentials', { triesLeft: 1 })).body, '1 try left before sign-in pauses for 15 minutes.');
  assert.equal(errorText(err(429, 'rate_limited', { retryAfterSeconds: 61 })).body, 'Try again in 2 minutes.');
  assert.equal(errorText(err(429, 'rate_limited', { retryAfterSeconds: 9 }), { provider: 'qwen' }).title, 'Qwen Token Plan was checked a moment ago');
  assert.equal(errorText(err(502, 'host_unreachable', { host: 'windows' })).title, 'Windows could not be reached');
  assert.equal(errorText(err(502, 'host_unreachable', { host: 'mac' })).title, 'The Mac could not be reached');
  assert.match(errorText(err(403, 'secure_transport_required', { fallback: { command: 'ai-account-center codex-auth login <profile-name>' } }), { provider: 'codex' }).body, /Or run on Ubuntu: ai-account-center codex-auth login/);
  assert.match(errorText(err(403, 'secure_transport_required')).body, /local network trust/);
  assert.equal(errorText(err(422, 'key_rejected'), { what: 'replace' }).body, 'The old key is still in use.');
  assert.equal(errorText(err(422, 'key_rejected')).body, 'Nothing was stored.');
  assert.equal(errorText(err(409, 'account_active'), { what: 'signin-again' }).body, 'Activate another account first, then sign in again.');
  assert.match(errorText(err(409, 'account_default'), { provider: 'claude' }).body, /default Claude profile/);
  assert.equal(errorText(err(400, 'weak_password', { reason: 'too_long' })).title, 'Password too long');
  // no code: by status, then the network
  assert.equal(errorText({ status: 500, payload: { error: 'The request could not be completed safely.' } }).title, 'That did not work');
  assert.match(errorText({ status: 500, payload: null }).body, /Nothing was changed/);
  assert.equal(errorText({ network: true, status: 0 }).title, 'Dashboard unreachable');
});

test('job failures, unavailable reasons and the local checks', () => {
  for (const code of ['tool_missing', 'unexpected_output', 'identity_mismatch', 'duplicate_identity', 'timeout', 'provider_denied', 'write_failed', 'server_restarted']) {
    assert.doesNotMatch(jobErrorText({ provider: 'codex', state: 'failed', error: { code } }), /did not finish/, code);
  }
  assert.equal(jobErrorText({ state: 'cancelled', error: null }), 'The sign-in was cancelled. Nothing was saved.');
  assert.deepEqual(unavailableText('not_implemented', 'muse').coming, true);
  for (const reason of ['secure_transport_required', 'tool_missing', 'preflight_failed', 'isolation_unproven', 'extension_update_required']) {
    const u = unavailableText(reason, 'codex');
    assert.equal(u.coming, false, reason);
    assert.ok(u.text, reason);
  }
  assert.equal(profileNameProblem(''), 'Name the profile first.');
  assert.match(profileNameProblem('bad name'), /letters, digits/);
  assert.match(profileNameProblem('Codex-1', ['codex-1']), /already a profile name/);
  assert.equal(profileNameProblem('codex-2', ['codex-1']), '');
  assert.match(claudeIdProblem('Party'), /lowercase/);
  assert.equal(claudeIdProblem('party'), '');
  assert.match(claudeIdProblem('party', ['party']), /already a Claude profile/);
  assert.equal(keyProblem('k-12345678'), '');
  assert.match(keyProblem('has space here'), /space/);
  assert.match(keyProblem('short'), /too short/);
  assert.match(keyProblem('x'.repeat(513)), /too long/);
  assert.match(keyProblem('ключ-12345678'), /character/);
  assert.equal(suggestName('codex', ['codex-2', 'one']), 'codex-3');
  assert.deepEqual(passwordProblem({ current: 'a', next: 'abcdefgh', confirm: 'abcdefgh' }), null);
  assert.deepEqual(passwordProblem({ current: '', next: 'abcdefgh', confirm: 'abcdefgh' })[0], 'cur');
  assert.deepEqual(passwordProblem({ current: 'a', next: 'x'.repeat(73), confirm: 'x'.repeat(73) })[0], 'new');
});

test('the password-change toast names the paired trays; sign out all devices says what happened', () => {
  assert.deepEqual(passwordChangedToast({ signedOutBrowsers: 2, pairedDevices: 2 }, [{ name: 'Mac tray', platform: 'mac' }, { name: 'Windows tray', platform: 'windows' }]),
    { title: 'Password changed. Mac tray and Windows tray stay signed in.', body: '2 other browsers were signed out; this one stays signed in.' });
  assert.equal(passwordChangedToast({ signedOutBrowsers: 0, pairedDevices: 1 }, [{ name: '', platform: 'windows' }]).title, 'Password changed. Windows tray stays signed in.');
  assert.equal(passwordChangedToast({ signedOutBrowsers: 0, pairedDevices: 0 }, []).title, 'Password changed.');
  assert.equal(passwordChangedToast({ pairedDevices: 3 }, []).title, 'Password changed. 3 paired trays stay signed in.');
  assert.deepEqual(revokeAllToast({ revokedDevices: 0, signedOutBrowsers: 0 }).body, 'No trays were paired. This browser stays signed in.');
});

test('copying a code falls back to a selection where the async clipboard is missing (plain HTTP)', async () => {
  const calls = [];
  const doc = { body: { appendChild: el => calls.push(['append', el.value]) }, createElement: () => ({ setAttribute() {}, style: {}, select: () => calls.push(['select']), remove: () => calls.push(['remove']) }), execCommand: cmd => { calls.push([cmd]); return true; } };
  assert.equal(await copyText('ABCD-12345', doc, {}), true);
  assert.deepEqual(calls.map(c => c[0]), ['append', 'select', 'copy', 'remove']);
  let written = '';
  assert.equal(await copyText('XY', doc, { clipboard: { writeText: async v => { written = v; } } }), true);
  assert.equal(written, 'XY');
});
