'use strict';
// Source-only fixtures: filesystem reads/writes occur only in fresh OS temp dirs;
// Python/PowerShell calls are mocked except one pure JSON-only Python stub
// launched for exact stdin/bootstrap composition; it does not read profile files.
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const crypto = require('node:crypto');
const assert = require('node:assert/strict');
const {MAC_PROFILE} = require('./synthetic-history-fixtures.cjs');
const bridge = require('../../../scripts/claude-history/history_index_node_bridge_v2.cjs');
const sha = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
const atomicSource = fs.readFileSync(path.join(__dirname, '../../../scripts/claude-history/history_index_transaction_v1.cjs'), 'utf8');
const syntheticGuard = '# SYNTHETIC_GUARD_SOURCE_ONLY_NOT_EXECUTED';
const syntheticGuardHash = sha(Buffer.from(syntheticGuard));
const cases = [];
const test = (name, run) => cases.push({name, run});
function fixture(count = 1) {
  const base = fs.mkdtempSync(path.join(os.tmpdir(), 'history-bridge-synthetic-'));
  const profileRoot = path.join(base, 'profile');
  const registryRoot = path.join(profileRoot, 'registry');
  fs.mkdirSync(profileRoot, {mode:0o700}); fs.mkdirSync(registryRoot, {mode:0o700});
  const protectedBytes = Buffer.from('SYNTHETIC_PRIVATE_PROTECTED');
  fs.writeFileSync(path.join(profileRoot, 'config.json'), protectedBytes, {mode:0o600});
  const executable = path.join(base, 'python3');
  const executableBytes = Buffer.from('SYNTHETIC_INTERPRETER_NEVER_EXECUTED');
  fs.writeFileSync(executable, executableBytes, {mode:0o600});
  const request = {schemaVersion:2, profileId:MAC_PROFILE,platform:'mac',policy:{synthetic:true},
    expectedTarget:{private:'SYNTHETIC_PRIVATE_TARGET'},profileRoot,registryRoot,
    protected:[{name:'config.json',base64:protectedBytes.toString('base64')},{name:'absent.json',base64:null}],
    records:Array.from({length:count}, (_, index) => {
      const bytes=Buffer.from(JSON.stringify({title:'SYNTHETIC_PRIVATE_TITLE', index}));
      return {name:'local_00000000-0000-4000-8000-' + (index+1).toString(16).padStart(12,'0') + '.json',base64:bytes.toString('base64'),sha256:sha(bytes)};
    }),transactionSource:atomicSource,guardSource:syntheticGuard,guardSourceSha256:syntheticGuardHash,
    pythonExecutable:executable,pythonExecutableSha256:sha(executableBytes)};
  const calls=[];
  const seams={guardSourceSha256:syntheticGuardHash,
    spawnSync(executable,args,options) {calls.push({executable,args,options});return {status:0,stdout:'{"unchanged":true}'};},
    queryAcl() {return {status:0,stdout:'{"private":true}'};}};
  return {base,profileRoot,registryRoot,request,seams,calls,protectedBytes,
    cleanup(){fs.rmSync(base,{recursive:true,force:true});}};
}
function withFixture(count, run) {const f=fixture(count);try {run(f);}finally {f.cleanup();}}
const targets=f=>fs.readdirSync(f.registryRoot).filter(name=>name.startsWith('local_'));
function publicOnly(result, f) {
  const encoded=JSON.stringify(result);
  for(const secret of [f.base,'SYNTHETIC_PRIVATE_PROTECTED','SYNTHETIC_PRIVATE_TARGET','SYNTHETIC_PRIVATE_TITLE',f.request.records[0]?.name]) {
    if(secret) assert.equal(encoded.includes(secret),false);
  }
  for(const key of ['reason','action','snapshotDirectory','createdSha256','policy','expectedTarget','guardSource','transactionSource']) assert.equal(Object.hasOwn(result,key),false);
}
test('production_default_pins_match_exact_packaged_helper_and_transaction',()=>{
  const actualHelper=fs.readFileSync(path.join(__dirname,'../../../scripts/claude-history/history_index_remote_v2.py'));
  assert.equal(bridge.pins.guardSourceSha256,sha(actualHelper));
  assert.equal(bridge.pins.transactionSha256,sha(Buffer.from(atomicSource)));
});
test('exact_packaged_helper_accepted_without_guard_pin_override',()=>withFixture(1,f=>{
  const actualHelper=fs.readFileSync(path.join(__dirname,'../../../scripts/claude-history/history_index_remote_v2.py'),'utf8');
  f.request.guardSource=actualHelper;f.request.guardSourceSha256=sha(Buffer.from(actualHelper));
  const {guardSourceSha256:ignored,...onlyReadonlyGuardSeams}=f.seams;
  const result=bridge.runBridge(f.request,onlyReadonlyGuardSeams);
  assert.equal(result.status,'created_metadata');assert.equal(result.createdCount,1);
  assert.equal(targets(f).length,1);assert.equal(f.calls.length>3,true);publicOnly(result,f);
}));
test('old_or_modified_guard_source_refuses_without_pin_override_or_childcalls',()=>withFixture(1,f=>{
  const actualHelper=fs.readFileSync(path.join(__dirname,'../../../scripts/claude-history/history_index_remote_v2.py'),'utf8');
  const {guardSourceSha256:ignored,...onlyReadonlyGuardSeams}=f.seams;
  f.request.guardSource=actualHelper+'\n# synthetic modified source';f.request.guardSourceSha256=sha(Buffer.from(f.request.guardSource));
  const result=bridge.runBridge(f.request,onlyReadonlyGuardSeams);
  assert.equal(result.status,'refused');assert.equal(f.calls.length,0);assert.equal(targets(f).length,0);
}));
for(const count of [1,18,32,200]) test('pinned_source_composition_creates_' + count + '_synthetic_records',()=>withFixture(count,f=>{
  const result=bridge.runBridge(f.request,f.seams);
  assert.equal(result.status,'created_metadata');assert.equal(result.createdCount,count);assert.equal(result.protectedBytesUnchanged,true);
  assert.equal(result.profileApplyEndpointImplemented,false);assert.equal(result.vendorResumeImplemented,false);
  assert.equal(targets(f).length,count);assert.equal(f.calls.length>count*3,true);
  for(const call of f.calls) {
    assert.equal(call.executable,f.request.pythonExecutable);
    assert.deepEqual(call.args,['-I','-c',bridge.fixedPrograms.pythonBootstrap]);
    assert.equal(call.options.shell,false);assert.equal(call.options.windowsHide,true);
    assert.equal(call.options.env.HOME,os.homedir());
    assert.equal(Object.hasOwn(call.options.env,'SYNTHETIC_PRIVATE_TARGET'),false);
    const envelope=JSON.parse(call.options.input);
    assert.equal(Buffer.from(envelope.guardSourceBase64,'base64').toString(),syntheticGuard);
    assert.deepEqual(Object.keys(envelope.guardRequest).sort(),['expectedTarget','mode','platform','policy','profileId','records']);
    assert.equal(envelope.guardRequest.mode,'protected-check');
    assert.deepEqual(envelope.guardRequest.records,f.request.records);
    assert.deepEqual(envelope.guardRequest.expectedTarget,f.request.expectedTarget);
  }
  assert.deepEqual(fs.readFileSync(path.join(f.profileRoot,'config.json')),f.protectedBytes);
  publicOnly(result,f);
}));
for(const [label, mutate] of Object.entries({transaction_source:r=>r.transactionSource+='\n// synthetic mutation',guard_source:r=>r.guardSource+=' mutation',guard_hash:r=>r.guardSourceSha256='0'.repeat(64),record_hash:r=>r.records[0].sha256='0'.repeat(64),profile:r=>r.profileId='unknown',platform:r=>r.platform='unknown',schema:r=>r.schemaVersion=1,policy:r=>r.policy=null,target:r=>r.expectedTarget=[]})) {
  test('mismatch_' + label + '_refuses_without_spawn_or_writes',()=>withFixture(1,f=>{
    mutate(f.request);const result=bridge.runBridge(f.request,f.seams);
    assert.equal(result.status,'refused');assert.equal(f.calls.length,0);assert.equal(targets(f).length,0);
    assert.equal(fs.readdirSync(f.profileRoot).length,2);publicOnly(result,f);
  }));
}
for(const [label,value] of Object.entries({whitespace:' Zg==',invalid:'!!!!',noncanonical:'Zh==',padding:'Zg=',type:[],oversized:'A'.repeat(2666668)})) {
  test('base64_' + label + '_refuses',()=>withFixture(1,f=>{
    f.request.records[0].base64=value;const result=bridge.runBridge(f.request,f.seams);
    assert.equal(result.status,'refused');assert.equal(f.calls.length,0);assert.equal(targets(f).length,0);
  }));
}
for(const [label,result] of Object.entries({duplicate:{status:0,stdout:'{"unchanged":false,"unchanged":true}'},false:{status:0,stdout:'{"unchanged":false}'},truthy:{status:0,stdout:'{"unchanged":1}'},extra:{status:0,stdout:'{"unchanged":true,"private":"SYNTHETIC"}'},noise:{status:0,stdout:'noise\n{"unchanged":true}'},array:{status:0,stdout:'[true]'},null:{status:0,stdout:'null'},failed:{status:1,stdout:'{"unchanged":true}'},signal:{status:0,signal:'SIGTERM',stdout:'{"unchanged":true}'},error:{status:0,error:new Error('SYNTHETIC_PRIVATE_ERROR'),stdout:'{"unchanged":true}'}})) {
  test('protected_guard_' + label + '_fails_closed',()=>withFixture(1,f=>{
    f.seams.spawnSync=()=>result;const output=bridge.runBridge(f.request,f.seams);
    assert.equal(output.status,'refused');assert.equal(targets(f).length,0);
    assert.equal(fs.readdirSync(f.profileRoot).length,2);publicOnly(output,f);
  }));
}
test('guard_exception_suppressed',()=>withFixture(1,f=>{
  f.seams.spawnSync=()=>{throw new Error('SYNTHETIC_SECRET_EXCEPTION_PATH');};
  const result=bridge.runBridge(f.request,f.seams);assert.equal(result.status,'refused');
  assert.equal(JSON.stringify(result).includes('SYNTHETIC_SECRET_EXCEPTION_PATH'),false);
}));
test('guard_rechecked_after_prior_success_before_copy',()=>withFixture(1,f=>{
  let calls=0;f.seams.spawnSync=()=>({status:0,stdout:++calls===3?'{"unchanged":false}':'{"unchanged":true}'});
  const result=bridge.runBridge(f.request,f.seams);
  assert.equal(result.status,'refused');assert.equal(calls>=3,true);assert.equal(targets(f).length,0);
}));
test('interpreter_hash_mismatch_refuses_before_guard_spawn',()=>withFixture(1,f=>{
  f.request.pythonExecutableSha256='0'.repeat(64);
  const result=bridge.runBridge(f.request,f.seams);assert.equal(result.status,'refused');
  assert.equal(f.calls.length,0);assert.equal(targets(f).length,0);
}));
test('interpreter_modified_after_success_rechecked',()=>withFixture(1,f=>{
  const original=f.seams.spawnSync;let first=true;
  f.seams.spawnSync=(...args)=>{const result=original(...args);if(first){first=false;fs.writeFileSync(f.request.pythonExecutable,'SYNTHETIC_REPLACED_INTERPRETER');}return result;};
  const result=bridge.runBridge(f.request,f.seams);assert.equal(result.status,'refused');
  assert.equal(f.calls.length,1);assert.equal(targets(f).length,0);
}));
test('interpreter_relative_path_refuses',()=>withFixture(1,f=>{
  f.request.pythonExecutable='python3';const result=bridge.runBridge(f.request,f.seams);
  assert.equal(result.status,'refused');assert.equal(f.calls.length,0);
}));
test('windows_injected_acl_readonly_policy_composition',()=>withFixture(1,f=>{
  f.request.platform='windows';const acl=[];f.seams.queryAcl=context=>{acl.push(context);return {status:0,stdout:'{"private":true}'};};
  const result=bridge.runBridge(f.request,f.seams);assert.equal(result.status,'created_metadata');
  assert.equal(acl.some(x=>x.phase==='beforePrivateContent'&&x.kind==='directory'),true);
  assert.equal(acl.some(x=>x.phase==='immediatelyBeforePrivateWrite'),true);
  assert.equal(acl.some(x=>x.phase==='immediatelyBeforePublication'),true);publicOnly(result,f);
}));
for(const [label,value] of Object.entries({duplicate:{status:0,stdout:'{"private":false,"private":true}'},unknown:{status:0,stdout:'{}'},false:{status:0,stdout:'{"private":false}'},extra:{status:0,stdout:'{"private":true,"sid":"SYNTHETIC"}'},error:{status:1,stdout:'{"private":true}'}})) {
  test('windows_acl_' + label + '_refuses_before_private_content',()=>withFixture(1,f=>{
    f.request.platform='windows';f.seams.queryAcl=()=>value;const result=bridge.runBridge(f.request,f.seams);
    assert.equal(result.status,'refused');assert.equal(targets(f).length,0);assert.equal(fs.readdirSync(f.profileRoot).length,2);
  }));
}
test('windows_acl_fixed_script_uses_owner_allowlist_and_no_acl_mutation',()=>{
  const script=bridge.fixedPrograms.aclScript;
  for(const token of ['Get-Acl','GetAccessRules','GetOwner','WindowsIdentity','S-1-5-18','S-1-5-32-544','ReparsePoint']) assert.equal(script.includes(token),true);
  for(const token of ['Set-Acl','SetAccessRule','AddAccessRule','icacls','Start-Process']) assert.equal(script.includes(token),false);
});
test('unknown_transaction_status_cannot_export_claims',()=>{
  const result=bridge.publicResult({status:'SYNTHETIC_UNKNOWN',snapshotBindingVerified:true,privateSnapshotsPreserved:true,reason:'SYNTHETIC_SECRET'});
  assert.equal(result.status,'refused');assert.equal(result.snapshotBindingVerified,false);assert.equal(result.privateSnapshotsPreserved,false);
  assert.equal(JSON.stringify(result).includes('SYNTHETIC_SECRET'),false);
});
test('rollback_report_allowlist_preserves_current_binding_unknown',()=>{
  const result=bridge.publicResult({status:'refused_replacement_preserved',ownedFilesRolledBack:false,
    createdCountBeforeRefusal:2,snapshotCreated:true,snapshotBindingVerified:false,privateSnapshotsPreserved:false,
    snapshotDirectory:'/SYNTHETIC_PRIVATE_PATH',reason:'SYNTHETIC_PRIVATE_REASON'});
  assert.equal(result.createdCountBeforeRefusal,2);assert.equal(result.recoveryRequired,true);
  assert.equal(result.snapshotBindingVerified,false);assert.equal(Object.hasOwn(result,'reason'),false);assert.equal(Object.hasOwn(result,'snapshotDirectory'),false);
});
test('rollback_with_unknown_snapshot_privacy_requires_recovery',()=>{
  const result=bridge.publicResult({status:'refused_rolled_back',ownedFilesRolledBack:true,snapshotCreated:true,snapshotBindingVerified:true,privateSnapshotsPreserved:false});
  assert.equal(result.ownedFilesRolledBack,true);assert.equal(result.recoveryRequired,true);
});
test('existing_destination_foreign_and_backup_preserved_public_refusal',()=>withFixture(1,f=>{
  const target=path.join(f.registryRoot,f.request.records[0].name);
  fs.writeFileSync(target,'SYNTHETIC_FOREIGN');fs.writeFileSync(target+'.bak','SYNTHETIC_BACKUP');
  const result=bridge.runBridge(f.request,f.seams);assert.equal(result.status,'refused');
  assert.equal(fs.readFileSync(target,'utf8'),'SYNTHETIC_FOREIGN');assert.equal(fs.readFileSync(target+'.bak','utf8'),'SYNTHETIC_BACKUP');publicOnly(result,f);
}));
test('explicit_recovery_required_survives_safe_rollback_normalization',()=>{
  const result=bridge.publicResult({status:'refused_rolled_back',ownedFilesRolledBack:true,snapshotCreated:false,recoveryRequired:true});
  assert.equal(result.recoveryRequired,true);
});
test('actual_spawned_python_json_guard_and_tempFS_transaction_composition',()=>withFixture(1,f=>{
  const source=[
    'import sys,json,base64,hashlib',
    'r=json.loads(sys.stdin.buffer.read())',
    `good=(r.get('mode')=='protected-check' and r.get('profileId')==${JSON.stringify(MAC_PROFILE)} and r.get('platform')=='mac' and r.get('expectedTarget')=={'private':'SYNTHETIC_PRIVATE_TARGET'})`,
    "good=good and all(hashlib.sha256(base64.b64decode(v['base64'],validate=True)).hexdigest()==v['sha256'] for v in r['records'])",
    "sys.stdout.write(json.dumps({'unchanged':good},separators=(',',':')))",
  ].join('\n');
  f.request.guardSource=source;f.request.guardSourceSha256=sha(Buffer.from(source));
  f.request.pythonExecutable=fs.realpathSync('/usr/bin/python3');
  f.request.pythonExecutableSha256=sha(fs.readFileSync(f.request.pythonExecutable));
  const result=bridge.runBridge(f.request,{guardSourceSha256:f.request.guardSourceSha256});
  assert.equal(result.status,'created_metadata');assert.equal(result.createdCount,1);
  assert.equal(result.protectedBytesUnchanged,true);assert.equal(targets(f).length,1);
  assert.deepEqual(fs.readFileSync(path.join(f.profileRoot,'config.json')),f.protectedBytes);
  publicOnly(result,f);
}));
test('exact_true_output_accepts_only_JSON_whitespace_around_single_literal',()=>{
  assert.equal(bridge.exactTrueOutput({status:0,stdout:' \r\n{ \t"unchanged"\r\n : true\n } \t'},'unchanged'),true);
  assert.equal(bridge.exactTrueOutput({status:0,stdout:' {"private": true}\n'},'private'),true);
  assert.equal(bridge.exactTrueOutput({status:0,stdout:'\u00a0{"unchanged":true}'},'unchanged'),false);
});
test('actual_short_Node_wrapper_invalid_request_emits_one_private_free_JSON_refusal',()=>{
  const helperSource=fs.readFileSync(path.join(__dirname,'../../../scripts/claude-history/history_index_remote_v2.py'),'utf8');
  const line=helperSource.split('\n').find(value=>value.trim().startsWith('code = "'));
  assert.equal(typeof line,'string');
  // The frozen helper's double-quoted bootstrap literal uses JSON-compatible
  // escapes. Decode that exact literal without importing/executing the helper.
  const code=JSON.parse(line.slice(line.indexOf('=')+1).trim());
  const bridgeSource=fs.readFileSync(path.join(__dirname,'../../../scripts/claude-history/history_index_node_bridge_v2.cjs'),'utf8');
  const result=require('node:child_process').spawnSync(process.execPath,['-e',code],{
    input:JSON.stringify({bridgeSource,request:{}}),encoding:'utf8',timeout:10000,maxBuffer:65536,shell:false,
  });
  assert.equal(result.status,0);assert.equal(result.error,undefined);assert.equal(result.stderr,'');
  const output=JSON.parse(result.stdout);
  assert.equal(output.status,'refused');assert.equal(output.createdCount,0);assert.equal(output.recoveryRequired,true);
  assert.equal(Object.hasOwn(output,'reason'),false);assert.equal(Object.hasOwn(output,'path'),false);
  assert.equal(result.stdout.trim(),JSON.stringify(output));
});
function run() {const results=[];for(const item of cases){try {item.run();results.push({name:item.name,status:'PASS'});}catch(error){results.push({name:item.name,status:'FAIL',error:String(error.stack)});}}
return {testCount:cases.length,passed:results.filter(x=>x.status==='PASS').length,failed:results.filter(x=>x.status==='FAIL').length,results};}
if(require.main===module){const result=run();console.log(JSON.stringify(result,null,2));process.exitCode=result.failed?1:0;}
module.exports={run};
