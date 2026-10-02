'use strict';

// Every value is invented for offline fixtures. Nothing comes from a profile,
// environment, installed app, provider, credential store or remote computer.
const crypto = require('node:crypto');
const digest = value => crypto.createHash('sha256').update(value).digest('hex');
const ROLES = Object.freeze(['platyr', 'gmail', 'me', 'party']);
const PRIVATE_TITLE = 'SYNTHETIC_PRIVATE_HISTORY_TITLE';
const PRIVATE_ERROR = 'SYNTHETIC_PRIVATE_ADAPTER_ERROR';
const uuid = n => '00000000-0000-4000-8000-' + n.toString(16).padStart(12, '0');

function bindings(tag = 'A') {
  const roleIdentity = role => {
    const ordinal = ROLES.indexOf(role) + 1;
    const accountUuid = uuid(1000 + tag.charCodeAt(0) * 10 + ordinal);
    const organizationUuid = uuid(2000 + tag.charCodeAt(0) * 10 + ordinal);
    return {accountUuid, organizationUuid,
      accountSha256: digest(accountUuid), orgSha256: digest(organizationUuid)};
  };
  const hostname = 'synthetic-host-' + tag;
  const username = 'synthetic-user-' + tag;
  const port = 2200 + tag.charCodeAt(0);
  return {
    tag,
    profiles: Object.fromEntries(ROLES.map(role => [role, roleIdentity(role)])),
    plainEndpoint: {hostname, username, port},
    endpoint: {
      hostnameSha256: digest(hostname), usernameSha256: digest(username), portSha256: digest(String(port)),
    },
    project: '/mnt/Cursor/PuppetMaster',
    aliases: {mac: 'synthetic-mac-alias-' + tag, windows: 'synthetic-windows-alias-' + tag},
    transcriptRoot: '/synthetic-user/.claude/projects',
    transcriptPrefix: '/synthetic-user/.claude/projects/',
  };
}

function record(seed, platform = 'mac', changes = {}, ordinal = 1) {
  const desktopId = uuid(ordinal * 2 - 1), cliId = uuid(ordinal * 2);
  return {
    sessionId: 'local_' + desktopId,
    cliSessionId: cliId,
    title: PRIVATE_TITLE,
    cwd: seed.project,
    originCwd: seed.project,
    sshConfig: {sshHost: seed.aliases[platform]},
    sshRemoteTranscriptPath: seed.transcriptPrefix + '-synthetic-project-' + seed.tag + '/' + cliId + '.jsonl',
    createdAt: 1, lastActivityAt: 2, isArchived: false,
    ...changes,
  };
}

function envelope(value) {
  const bytes = Buffer.from(JSON.stringify(value));
  return {name: value.sessionId + '.json', bytes, sha256: digest(bytes)};
}

function immutableBinding(value) {
  return {
    sessionId: value.sessionId, cliSessionId: value.cliSessionId,
    cwd: value.cwd, originCwd: value.originCwd,
    sshHost: value.sshConfig?.sshHost,
    transcript: value.sshRemoteTranscriptPath,
  };
}

module.exports = {ROLES, PRIVATE_TITLE, PRIVATE_ERROR, digest, uuid,
  bindings, record, envelope, immutableBinding};
