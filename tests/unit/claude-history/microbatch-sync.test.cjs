'use strict';
// External proposal fixtures only. All data/inodes are disposable tempFS;
// vendor/process/native/SSH proofs use explicit synthetic seams. No live input.
const test=require('node:test'),assert=require('node:assert/strict');
const fs=require('node:fs'),path=require('node:path'),os=require('node:os');
const sync=require('../../../scripts/claude-history/history-index-sync.cjs');
const fx=require('./synthetic-history-fixtures.cjs');
const ROOT=path.resolve(__dirname,'../../..');
const {transaction}=require(path.join(ROOT,'scripts/claude-history/history_index_transaction_v1.cjs'));
function fixture(count=18,existing=18){
 const directory=fs.mkdtempSync(path.join(os.tmpdir(),'history-microbatch-'));
 fs.chmodSync(directory,0o700);
 const profile=path.join(directory,'profile'),registry=path.join(profile,'registry');
 fs.mkdirSync(profile,{mode:0o700});fs.mkdirSync(registry,{mode:0o700});
 const seed=fx.bindings('A'),identity=seed.profiles.gmail;
 const policy={version:1,enabled:true,sourcePlatform:'windows',identity:{accountUuid:identity.accountUuid,organizationUuid:identity.organizationUuid},project:{cwd:seed.project,originCwd:seed.project,transcriptRoot:seed.transcriptRoot},ssh:Object.fromEntries(['mac','windows'].map(p=>[p,{alias:seed.aliases[p],...seed.plainEndpoint}]))};
 const protectedBytes=Buffer.from(JSON.stringify({lastKnownAccountUuid:identity.accountUuid}));
 fs.writeFileSync(path.join(profile,'config.json'),protectedBytes,{mode:0o600});
 const protectedState={files:[{relative:'config.json',sha256:fx.digest(protectedBytes)}],directoryInodes:{profile:fs.statSync(profile).ino,registry:fs.statSync(registry).ino}};
 const baseline=Array.from({length:existing},(_,i)=>fx.envelope(fx.record(seed,'mac',{},i+1000)));
 for(const row of baseline)fs.writeFileSync(path.join(registry,row.name),row.bytes,{mode:0o600});
 const sourceRows=Array.from({length:count},(_,i)=>fx.envelope(fx.record(seed,'windows',{},i+1)));
 const snapshot=(platform,records)=>({profileId:'gmail',platform,identity:{accountSha256:identity.accountSha256,orgSha256:identity.orgSha256},endpoint:seed.endpoint,records,snapshotStable:true,revision:sync.snapshotRevision(records),nativeGuard:{...sync.NATIVE[platform],warmGuardVerified:true,autoResumeGuardVerified:true},noPendingInput:true,noScheduledWork:true,protectedSnapshotStable:true,protectedSnapshot:protectedState});
 const target=()=>snapshot('mac',fs.readdirSync(registry).filter(n=>/^local_.*\.json$/.test(n)).sort().map(name=>{const bytes=fs.readFileSync(path.join(registry,name));return{name,bytes,sha256:fx.digest(bytes)}}));
 const state={appendCalls:0,aggregateProgress:[],closedCalls:0,readTargetCalls:0,fullTransactionGuardCalls:0,afterAppend:null,appendFailure:null,targetMutation:null,marker:null};
 const adapters={
  async isTargetClosed(){state.closedCalls++;return state.closed!==false},
  async readSource(){return snapshot('windows',sourceRows)},
  async readTarget(){state.readTargetCalls++;const value=target();if(state.targetMutation&&state.appendCalls)state.targetMutation(value);return value},
  async verifyTranscriptReferences(){return true},
  async targetProtectedStateUnchanged(expected){return fs.readFileSync(path.join(profile,'config.json')).equals(protectedBytes)&&JSON.stringify(target().protectedSnapshot)===JSON.stringify(expected.protectedSnapshot)},
  async appendCreateOnly(payload){
   state.appendCalls++;assert.equal(payload.records.length,1);state.aggregateProgress.push([payload.totalCount,payload.confirmedCount]);await payload.closedGuard();
   // Same as the service: one durable marker per Open, re-armed per batch.
   if(state.marker)state.marker.rearm();else state.marker=sync.armPendingMarker(directory,'gmail','mac');
   const marker=state.marker;
   {
    marker.assertBound();
    if(state.appendFailure&&state.appendCalls===state.appendFailure.at){
     if(state.appendFailure.kind==='lost')throw Error(fx.PRIVATE_ERROR);
     const refusal={status:'refused',createdCount:0,recoveryRequired:false,ownedFilesRolledBack:true,writerQuiescent:true};marker.finish(refusal);return refusal;
    }
    const planned=new Map([...payload.target.records,...payload.records.map(r=>({...r,sha256:fx.digest(r.bytes)}))].map(r=>[r.name,r.bytes]));
    const baselineNames=new Set(payload.target.records.map(r=>r.name));
    const closedGuard=()=>{
     state.fullTransactionGuardCalls++;
     if(state.closed===false||!fs.readFileSync(path.join(profile,'config.json')).equals(protectedBytes))return false;
     const fresh=target();
     if(JSON.stringify(fresh.protectedSnapshot)!==JSON.stringify(payload.target.protectedSnapshot))return false;
     const present=new Set(fresh.records.map(r=>r.name));
     if([...baselineNames].some(name=>!present.has(name)))return false;
     return fresh.records.every(r=>planned.get(r.name)?.equals(r.bytes));
    };
    const result=transaction({profileRoot:profile,registryRoot:registry,records:payload.records,protected:[{name:'config.json',bytes:protectedBytes}],closedGuard,platform:'darwin'});
    assert.equal(result.status,'created_metadata');assert.equal(result.createdCount,1);
    const terminal={...result,writerQuiescent:true};marker.finish(terminal);
    if(state.afterAppend)state.afterAppend();return terminal;
   }
  },
 };
 const markers=()=>{const root=path.join(directory,'claude-history-pending');return fs.existsSync(root)?fs.readdirSync(root):[]};
 const run=async()=>{try{return await sync.synchronizeBeforeProfileOpen({profileId:'gmail',targetPlatform:'mac',policy,adapters})}finally{state.marker?.release();state.marker=null}};
 return {directory,profile,registry,state,adapters,policy,baseline,sourceRows,target,markers,run,cleanup:()=>fs.rmSync(directory,{recursive:true,force:true})};
}
async function withFixture(count,existing,run){const f=fixture(count,existing);try{await run(f)}finally{f.cleanup()}}
for(const count of [18,32])test('real tempFS composition appends all '+count+' in one-record guarded transactions',async()=>withFixture(count,count,async f=>{
 const original=f.baseline.map(r=>({...r,bytes:Buffer.from(r.bytes)}));const result=await f.run();
 assert.equal(result.status,'synchronized');assert.equal(result.createdCount,count);assert.equal(f.state.appendCalls,count);
 assert.equal(f.target().records.length,count*2);assert.equal(f.state.readTargetCalls,count);
 assert.deepEqual(f.state.aggregateProgress,Array.from({length:count},(_,i)=>[count,i]));
 assert.equal(sync.pendingMarkerState(f.directory,'gmail','mac').held,false);
 for(const row of original)assert.deepEqual(fs.readFileSync(path.join(f.registry,row.name)),row.bytes);
 for(const row of f.sourceRows){const value=JSON.parse(fs.readFileSync(path.join(f.registry,row.name)));assert.equal(value.cliSessionId,JSON.parse(row.bytes).cliSessionId);assert.equal(value.resumeConfirmed,false);assert.equal(value.permissionMode,'default');assert.equal(value.remoteControlAutoEligible,false);assert.equal(value.sshConfig.sshHost,fx.bindings('A').aliases.mac)}
 assert.equal(result.resumedSessions,0);assert.equal(result.copiedTranscripts,0);assert.equal(JSON.stringify(result).includes(fx.PRIVATE_TITLE),false);
 assert.ok(f.state.fullTransactionGuardCalls>=count*12);
 assert.equal(f.markers().length,1);
}));
test('lost second acknowledgement preserves first commit and durable hold, never invokes batch3',async()=>withFixture(18,18,async f=>{
 f.state.appendFailure={at:2,kind:'lost'};const result=await f.run();assert.equal(result.reason,'create_only_transaction_unconfirmed');assert.equal(result.createdCount,1);assert.equal(result.recoveryRequired,true);assert.equal(f.state.appendCalls,2);assert.equal(f.target().records.length,19);assert.equal(sync.pendingMarkerState(f.directory,'gmail','mac').held,true);
 assert.equal(f.markers().length,1);
}));
test('clean second refusal preserves confirmed count without unknown-writer claim',async()=>withFixture(18,18,async f=>{
 f.state.appendFailure={at:2,kind:'clean'};const result=await f.run();assert.equal(result.reason,'create_only_transaction_refused');assert.equal(result.createdCount,1);assert.equal(result.recoveryRequired,false);assert.equal(f.state.appendCalls,2);assert.equal(sync.pendingMarkerState(f.directory,'gmail','mac').held,false);
}));
for(const [name,change] of [
 ['unknown new record',f=>fs.writeFileSync(path.join(f.registry,'local_99999999-9999-4999-8999-999999999999.json'),JSON.stringify(fx.record(fx.bindings('A'),'mac',{},9999)),{mode:0o600})],
 ['original descriptor bytes',f=>fs.appendFileSync(path.join(f.registry,f.baseline[0].name),' ')],
 ['confirmed descriptor bytes',f=>fs.appendFileSync(path.join(f.registry,f.sourceRows[0].name),' ')],
 ['protected identity bytes',f=>fs.appendFileSync(path.join(f.profile,'config.json'),' ')],
 ['target opens',f=>{f.state.closed=false}],
 ])test(name+' drift stops before next append and reports confirmed partial count',async()=>withFixture(18,18,async f=>{
 f.state.afterAppend=()=>change(f);const result=await f.run();assert.equal(result.status,'refused');assert.equal(result.createdCount,1);assert.equal(f.state.appendCalls,1);assert.equal(sync.pendingMarkerState(f.directory,'gmail','mac').held,false);
}));
for(const [name,mutate] of [
 ['account',v=>{v.identity.accountSha256='0'.repeat(64)}],
 ['organization',v=>{v.identity.orgSha256='0'.repeat(64)}],
 ['endpoint',v=>{v.endpoint={...v.endpoint,hostnameSha256:'0'.repeat(64)}}],
 ['native guard',v=>{v.nativeGuard={...v.nativeGuard,managerSha256:'0'.repeat(64)}}],
 ['pending input',v=>{v.noPendingInput=false}],
 ['scheduled work',v=>{v.noScheduledWork=false}],
 ['protected stamp',v=>{v.protectedSnapshot={changed:true}}],
 ['snapshot stability',v=>{v.snapshotStable=false}],
 ['duplicate record',v=>{v.records.push(v.records[0]);v.revision=sync.snapshotRevision(v.records)}],
 ['missing original record',v=>{v.records=v.records.slice(1);v.revision=sync.snapshotRevision(v.records)}],
 ])test('fresh '+name+' proof must remain exact between batches',async()=>withFixture(18,18,async f=>{
 f.state.targetMutation=mutate;const result=await f.run();assert.equal(result.status,'refused');assert.equal(result.createdCount,1);assert.equal(f.state.appendCalls,1);
}));
test('registry 200 bound refuses before creating any pending marker or descriptor',async()=>withFixture(32,180,async f=>{
 const result=await f.run();assert.equal(result.reason,'registry_invalid');assert.equal(f.state.appendCalls,0);assert.equal(f.target().records.length,180);assert.equal(sync.pendingMarkerState(f.directory,'gmail','mac').held,false);
}));
test('registry bound accepts exactly 200 rows in one-record batches',async()=>withFixture(32,168,async f=>{
 assert.equal(sync.REGISTRY_LIMIT,200);assert.equal(sync.MICROBATCH_RECORDS,1);
 const result=await f.run();assert.equal(result.status,'synchronized');assert.equal(result.createdCount,32);
 assert.equal(f.state.appendCalls,32);assert.equal(f.target().records.length,200);
 assert.equal(sync.pendingMarkerState(f.directory,'gmail','mac').held,false);assert.equal(f.markers().length,1);
}));
test('between-batch read failure keeps confirmed rows without a hold or another append',async()=>withFixture(18,18,async f=>{
 const read=f.adapters.readTarget;f.adapters.readTarget=async()=>{if(f.state.appendCalls)throw Error(fx.PRIVATE_ERROR);return read()};
 const result=await f.run();assert.equal(result.status,'refused');assert.equal(result.reason,'unavailable');assert.equal(result.createdCount,1);
 assert.equal(result.recoveryRequired,false);assert.equal(f.state.appendCalls,1);assert.equal(f.target().records.length,19);
 assert.equal(JSON.stringify(result).includes(fx.PRIVATE_ERROR),false);assert.equal(sync.pendingMarkerState(f.directory,'gmail','mac').held,false);
}));
test('appendCreateOnly receives whole-plan progress counts with every one-record batch',async()=>withFixture(4,4,async f=>{
 const seen=[];const append=f.adapters.appendCreateOnly;f.adapters.appendCreateOnly=async payload=>{seen.push(Object.keys(payload).sort().join(','));return append(payload)};
 const result=await f.run();assert.equal(result.createdCount,4);assert.deepEqual(f.state.aggregateProgress,[[4,0],[4,1],[4,2],[4,3]]);
 assert.deepEqual([...new Set(seen)],['closedGuard,confirmedCount,profileId,records,target,targetPlatform,totalCount']);
}));
