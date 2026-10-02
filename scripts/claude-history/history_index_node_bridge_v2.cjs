'use strict';
/* Source-only transport bridge. Trusted packaged helper supplies private stdin.
 * Production never supplies offline seams. No app launch, resume or provider API.
 */
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const crypto = require('node:crypto');
const child = require('node:child_process');
const vm = require('node:vm');
const TRANSACTION_SHA256 = '0b3b4cbb14db144457684bf7683d53bbbc38529d4b92c9c6590ab089eff0c132';
// Exact frozen packaged Python helper source; no request-supplied pin in production.
const GUARD_SOURCE_SHA256 = 'aeb7718c5336ff7b71157a21a9bba6cba8f6d12be0bcce098ca1c1604857ec0c';
const MAX_INPUT_BYTES = 32000000;
const MAX_FILE_BYTES = 2000000;
const PYTHON_BOOTSTRAP = "import sys,json,base64,io; e=json.load(sys.stdin); sys.stdin=io.TextIOWrapper(io.BytesIO(json.dumps(e['guardRequest'],separators=(',',':')).encode('utf-8')),encoding='utf-8'); exec(compile(base64.b64decode(e['guardSourceBase64'],validate=True),'<fixed-history-protected-guard>','exec'),{'__name__':'__main__'})";
const POWERSHELL = 'C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe';
const ACL_SCRIPT = String.raw`
$ErrorActionPreference='Stop'
[Console]::InputEncoding=[System.Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false)
try {
  $q=([Console]::In.ReadToEnd() | ConvertFrom-Json)
  if (($q.kind -ne 'file') -and ($q.kind -ne 'directory')) { throw 'refused' }
  $item=Get-Item -LiteralPath ([string]$q.path) -Force -ErrorAction Stop
  if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'refused' }
  $parent=[IO.Path]::GetDirectoryName($item.FullName)
  while ($parent) {
    $parentItem=Get-Item -LiteralPath $parent -Force -ErrorAction Stop
    if (-not $parentItem.PSIsContainer -or (($parentItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)) { throw 'refused' }
    $next=[IO.Path]::GetDirectoryName($parent)
    if ($next -eq $parent) { break }
    $parent=$next
  }
  if (($q.kind -eq 'directory') -ne [bool]$item.PSIsContainer) { throw 'refused' }
  $acl=Get-Acl -LiteralPath ([string]$q.path) -ErrorAction Stop
  if (-not $acl.AreAccessRulesCanonical) { throw 'refused' }
  $sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value
  $owner=$acl.GetOwner([Security.Principal.SecurityIdentifier]).Value
  if ($owner -ne $sid) { throw 'refused' }
  $allowed=@($sid,'S-1-5-18','S-1-5-32-544')
  $userAllow=$false
  $rules=$acl.GetAccessRules($true,$true,[Security.Principal.SecurityIdentifier])
  if ($rules.Count -lt 1) { throw 'refused' }
  foreach ($rule in $rules) {
    if ($rule.AccessControlType -eq [Security.AccessControl.AccessControlType]::Allow) {
      if ($allowed -notcontains $rule.IdentityReference.Value) { throw 'refused' }
      if (($rule.IdentityReference.Value -eq $sid) -and (($rule.FileSystemRights -band [Security.AccessControl.FileSystemRights]::FullControl) -eq [Security.AccessControl.FileSystemRights]::FullControl)) { $userAllow=$true }
    }
  }
  if (-not $userAllow) { throw 'refused' }
  [Console]::Out.Write('{"private":true}')
} catch { [Console]::Out.Write('{"private":false}'); exit 1 }
`;
const sha = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
const ownObject = value => value !== null && typeof value === 'object' && !Array.isArray(value);
const hexSha = value => typeof value === 'string' && /^[0-9a-f]{64}$/.test(value);
const defaultRefusal = () => ({ schemaVersion: 2, status: 'refused', createdCount: 0,
  recoveryRequired: true, ownedFilesRolledBack: false, snapshotBindingVerified: false,
  privateSnapshotsPreserved: false, profileApplyEndpointImplemented: false,
  vendorResumeImplemented: false, distributedLeaseImplemented: false });
function canonicalBase64(value) {
  if (typeof value !== 'string' || value.length > Math.ceil(MAX_FILE_BYTES / 3) * 4 ||
      value.length % 4 !== 0 || !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(value)) throw new Error('refused');
  const bytes = Buffer.from(value, 'base64');
  if (bytes.length > MAX_FILE_BYTES || bytes.toString('base64') !== value) throw new Error('refused');
  return bytes;
}
function exactTrueOutput(result, key) {
  if (!result || result.error || result.signal || result.status !== 0 || typeof result.stdout !== 'string' || result.stdout.length > 4096) return false;
  try {
    if (!['unchanged', 'private'].includes(key)) return false;
    const exactLiteral = new RegExp('^[ \\t\\r\\n]*\\{[ \\t\\r\\n]*"' + key + '"[ \\t\\r\\n]*:[ \\t\\r\\n]*true[ \\t\\r\\n]*\\}[ \\t\\r\\n]*$');
    if (!exactLiteral.test(result.stdout)) return false;
    const value = JSON.parse(result.stdout);
    return ownObject(value) && Object.keys(value).length === 1 && Object.hasOwn(value, key) && value[key] === true;
  } catch (_) { return false; }
}
function verifyExecutable(executable, expectedHash) {
  if (typeof executable !== 'string' || !path.isAbsolute(executable) || executable.includes('\0') || !hexSha(expectedHash)) return false;
  let fd;
  try {
    const resolved = fs.realpathSync(executable);
    const before = fs.statSync(resolved, { bigint: true });
    if (!before.isFile() || before.dev <= 0n || before.ino <= 0n || before.size <= 0n || before.size > 50000000n) return false;
    fd = fs.openSync(resolved, fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW || 0));
    const opened = fs.fstatSync(fd, { bigint: true });
    if (opened.dev !== before.dev || opened.ino !== before.ino) return false;
    const bytes = fs.readFileSync(fd);
    const after = fs.statSync(executable, { bigint: true });
    return bytes.length <= 50000000 && after.dev === before.dev && after.ino === before.ino && sha(bytes) === expectedHash;
  } catch (_) { return false; }
  finally { if (fd !== undefined) fs.closeSync(fd); }
}
function publicResult(result) {
  if (!ownObject(result)) return defaultRefusal();
  const count = value => Number.isSafeInteger(value) && value >= 0 && value <= 200 ? value : 0;
  if (result.status === 'created_metadata' && count(result.createdCount) >= 1 && result.createOnly === true && result.protectedBytesUnchanged === true) {
    return { schemaVersion: 2, status: 'created_metadata', createdCount: count(result.createdCount), protectedBytesUnchanged: true,
      recoveryRequired: false, ownedFilesRolledBack: false, snapshotBindingVerified: true,
      privateSnapshotsPreserved: true, profileApplyEndpointImplemented: false,
      vendorResumeImplemented: false, distributedLeaseImplemented: false };
  }
  if (!['refused_rolled_back', 'refused_replacement_preserved'].includes(result.status)) return defaultRefusal();
  const safe = result.status === 'refused_rolled_back' && result.ownedFilesRolledBack === true;
  return { schemaVersion: 2, status: 'refused', createdCount: 0,
    createdCountBeforeRefusal: count(result.createdCountBeforeRefusal),
    recoveryRequired: result.recoveryRequired === true || !safe || (result.snapshotCreated === true && (result.snapshotBindingVerified !== true || result.privateSnapshotsPreserved !== true)),
    ownedFilesRolledBack: safe,
    snapshotBindingVerified: result.snapshotBindingVerified === true,
    privateSnapshotsPreserved: result.privateSnapshotsPreserved === true,
    profileApplyEndpointImplemented: false, vendorResumeImplemented: false, distributedLeaseImplemented: false };
}
function runBridge(request, offlineSeams = {}) {
  try {
    if (!ownObject(request) || request.schemaVersion !== 2 || !['mac', 'windows'].includes(request.platform) ||
        !['platyr', 'gmail', 'party', 'me'].includes(request.profileId) || !ownObject(request.policy) || !ownObject(request.expectedTarget)) return defaultRefusal();
    const guardHash = offlineSeams.guardSourceSha256 || GUARD_SOURCE_SHA256;
    if (typeof request.transactionSource !== 'string' || sha(Buffer.from(request.transactionSource)) !== TRANSACTION_SHA256 ||
        typeof request.guardSource !== 'string' || request.guardSource.length > 2000000 ||
        !hexSha(guardHash) || request.guardSourceSha256 !== guardHash || sha(Buffer.from(request.guardSource)) !== guardHash ||
        typeof request.pythonExecutable !== 'string' || !hexSha(request.pythonExecutableSha256)) return defaultRefusal();
    if (!Array.isArray(request.records) || request.records.length < 1 || request.records.length > 200 ||
        !Array.isArray(request.protected) || request.protected.length > 512) return defaultRefusal();
    const records = request.records.map(record => {
      if (!ownObject(record) || typeof record.name !== 'string' || !hexSha(record.sha256)) throw new Error('refused');
      const bytes = canonicalBase64(record.base64);
      if (sha(bytes) !== record.sha256) throw new Error('refused');
      return { name: record.name, bytes };
    });
    const protectedRecords = request.protected.map(record => {
      if (!ownObject(record) || typeof record.name !== 'string') throw new Error('refused');
      return { name: record.name, bytes: record.base64 === null ? null : canonicalBase64(record.base64) };
    });
    const spawn = offlineSeams.spawnSync || child.spawnSync;
    const executableGuard = offlineSeams.verifyExecutable || verifyExecutable;
    const protectedRequest = { mode: 'protected-check', profileId: request.profileId,
      platform: request.platform, policy: request.policy, expectedTarget: request.expectedTarget, records: request.records };
    const guardInput = JSON.stringify({ guardSourceBase64: Buffer.from(request.guardSource).toString('base64'), guardRequest: protectedRequest });
    const home = os.homedir();
    const drive = path.win32.parse(home).root.replace(/[\\/]$/, '');
    const pythonEnv = process.platform === 'win32' ? {
      SystemRoot: 'C:\\Windows', WINDIR: 'C:\\Windows', USERPROFILE: home,
      HOMEDRIVE: drive, HOMEPATH: home.slice(drive.length),
    } : { HOME: home };
    const closedGuard = () => {
      if (!executableGuard(request.pythonExecutable, request.pythonExecutableSha256)) return false;
      let result;
      try { result = spawn(request.pythonExecutable, ['-I', '-c', PYTHON_BOOTSTRAP], {
        input: guardInput, encoding: 'utf8', timeout: 30000, maxBuffer: 8192, windowsHide: true,
        shell: false, env: pythonEnv,
      }); } catch (_) { return false; }
      return exactTrueOutput(result, 'unchanged');
    };
    const privateStorageGuard = request.platform === 'windows' ? context => {
      const query = offlineSeams.queryAcl || (value => spawn(POWERSHELL,
        ['-NoLogo', '-NoProfile', '-NonInteractive', '-Command', ACL_SCRIPT], {
          input: JSON.stringify({ path: value.path, kind: value.kind }), encoding: 'utf8',
          timeout: 15000, maxBuffer: 8192, windowsHide: true, shell: false,
          env: { SystemRoot: 'C:\\Windows', WINDIR: 'C:\\Windows' },
        }));
      try { return exactTrueOutput(query(context), 'private'); } catch (_) { return false; }
    } : undefined;
    const packaged = { exports: {} };
    const wrapper = vm.runInThisContext('(function(require,module,exports){\n' + request.transactionSource + '\n})', { filename: '<sha-pinned-history-transaction>' });
    wrapper(require, packaged, packaged.exports);
    if (typeof packaged.exports.transaction !== 'function') return defaultRefusal();
    const result = packaged.exports.transaction({ profileRoot: request.profileRoot, registryRoot: request.registryRoot,
      records, protected: protectedRecords, closedGuard, privateStorageGuard,
      platform: request.platform === 'windows' ? 'win32' : 'darwin' });
    return publicResult(result);
  } catch (_) { return defaultRefusal(); }
}
function main() {
  let result = defaultRefusal();
  try {
    const chunks = []; let size = 0;
    const buf = Buffer.alloc(65536);
    while (true) {
      const count = fs.readSync(0, buf, 0, buf.length, null);
      if (!count) break;
      size += count; if (size > MAX_INPUT_BYTES) throw new Error('refused');
      chunks.push(Buffer.from(buf.subarray(0, count)));
    }
    result = runBridge(JSON.parse(Buffer.concat(chunks).toString('utf8')));
  } catch (_) {}
  process.stdout.write(JSON.stringify(result));
}
module.exports = { runBridge, publicResult, exactTrueOutput, canonicalBase64, verifyExecutable,
  pins: { transactionSha256: TRANSACTION_SHA256, guardSourceSha256: GUARD_SOURCE_SHA256 },
  fixedPrograms: { pythonBootstrap: PYTHON_BOOTSTRAP, powershell: POWERSHELL, aclScript: ACL_SCRIPT } };
if (require.main === module) main();
