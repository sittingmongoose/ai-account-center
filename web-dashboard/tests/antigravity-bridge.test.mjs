import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
// Exercise the real browser bridge with synthetic HTTP and Slint-export adapters.
// This is controller/data proof, not native canvas or browser visual QA.
test('bridge routes Ubuntu approvals separately, guards Codex, and writes only confirmed independent auto settings', async () => {
  const base = new URL('../public/', import.meta.url);
  let source = await readFile(new URL('bridge.js', base), 'utf8');
  const declarations = source.split('\n')[0].match(/\{ ([^}]+) \}/)[1];
  source = source.replace(source.split('\n')[0], `const init = async () => {}; const { ${declarations} } = globalThis.__agBridgeWasm;`)
    .replace(/import \{ requireWebGL, WEBGL_REQUIRED_MESSAGE, startSlintDashboard \} from '[^']+';/, 'const requireWebGL = () => {}; const WEBGL_REQUIRED_MESSAGE = "fixture"; const startSlintDashboard = fn => fn();')
    .replace(/from '(\.\/[^']+)'/g, (_match, relative) => `from '${new URL(relative, base).href}'`);
  const originals = new Map();
  const assign = (name, value) => { originals.set(name, Object.getOwnPropertyDescriptor(globalThis, name)); Object.defineProperty(globalThis, name, {value, configurable:true, writable:true}); };
  const restore = () => { for (const [name, descriptor] of originals) descriptor ? Object.defineProperty(globalThis, name, descriptor) : delete globalThis[name]; };
  const calls = [], models = [], offers = [];
  const now = Date.now();
  const profiles = ['gmail', 'party'].map((id,index) => ({id,email:`${id}@example.com`,plan:'Pro',hostId:'ubuntu',available:true,selected:index===0,runtimeVerified:index===0,verifiedAt:new Date(now).toISOString()}));
  const accounts = profiles.map(p => ({id:`antigravity:profile:${p.id}`,provider:'antigravity',providerLabel:'Antigravity',email:p.email,platform:'ubuntu',status:'ok',sampledAt:new Date(now).toISOString(),source:'Fixture native consumer',isActive:p.selected,
    capabilities:{antigravityProfileId:p.id,antigravityHostIds:['ubuntu'],antigravityCanActivate:true},
    windows:[300,10080].map(minutes=>({key:`gemini-${minutes}`,label:`Gemini ${minutes}`,kind:'rate_limit',windowMinutes:minutes,usedPercent:0,resetAt:new Date(now+minutes*60000).toISOString(),poolId:'reported@gemini',poolIdSource:'provider-bucket-membership',poolLabel:'Gemini models'}))}));
  accounts.push({id:'codex:other',provider:'codex',email:'other@example.com',isActive:false,capabilities:{codexProfile:'other'},windows:[]});
  let auto = {enabled:false,thresholdUsedPercent:95,pollIntervalSeconds:60,maxQuotaAgeSeconds:300,cooldownSeconds:300,selectedHostIds:['ubuntu'],requestedPoolId:null,outcome:'setup_required',message:'Fixture setup',activationInProgress:false};
  const token = 'FixtureTokenForAntigravity12345';
  let confirmationPosts = 0;
  assign('__agBridgeWasm', Object.fromEntries(declarations.split(', ').map(name=>[name, name==='set_dashboard' ? value=>models.push(JSON.parse(value)) : name==='show_activation_confirmation' ? value=>offers.push(JSON.parse(value)) : ()=>{}])));
  assign('window',{}); assign('navigator',{userAgent:'Windows fixture'}); assign('location',{pathname:'/',search:''}); assign('document',{querySelector:()=>({hidden:false,textContent:''})});
  assign('innerWidth',1920); assign('innerHeight',1080); assign('devicePixelRatio',1); assign('addEventListener',()=>{}); assign('matchMedia',()=>({matches:true})); assign('localStorage',{getItem:()=> 'dark',setItem:()=>{}}); assign('setInterval',()=>1); assign('clearInterval',()=>{});
  assign('fetch',async(path, options={})=> {
    const body=options.body ? JSON.parse(options.body) : null; calls.push({path,method:options.method||'GET',body,credentials:options.credentials});
    let payload, status=200;
    if(path==='/api/auth/check') payload={authenticated:true};
    else if(path==='/api/accounts/settings') payload={refreshIntervalSeconds:60};
    else if(path.startsWith('/api/accounts/dashboard?')) payload={schemaVersion:1,updatedAt:new Date(now).toISOString(),accounts,settings:{refreshIntervalSeconds:60}};
    else if(path==='/api/claude/desktop-profiles') payload={profiles:[]};
    else if(path==='/api/antigravity/profiles') payload={schemaVersion:1,hostId:'ubuntu',activationSupported:true,profiles};
    else if(path==='/api/antigravity/auto-switch') { if(body) auto={...auto,...body}; payload=auto; }
    else if(path==='/api/app-updates/status') payload={job:null};
    else if(path==='/api/antigravity/profiles/party/activate') { status=409;payload={status:'confirmation-required',profileId:'party',hostId:'ubuntu',email:'party@example.com',confirmation:{token,profileId:'party',hostId:'ubuntu',email:'party@example.com',expiresAt:new Date(now+60000).toISOString(),processes:[{pid:456,role:'cli',label:'Antigravity CLI'}]}}; }
    else if(path==='/api/antigravity/profiles/party/confirm') { confirmationPosts++;profiles[0].selected=false;profiles[0].runtimeVerified=false;profiles[1].selected=true;profiles[1].runtimeVerified=true;payload={status:'active',profileId:'party',hostId:'ubuntu',email:'party@example.com'}; }
    else throw new Error(`Unexpected fixture request: ${path}`);
    return {ok:status===200,status,json:async()=>payload};
  });
  try {
    await import(`data:text/javascript;base64,${Buffer.from(source).toString('base64')}`);
    assert.equal(models.at(-1).antigravityAccounts.length,2); assert.equal(models.at(-1).antigravityAutoEnabled,false);
    const action=window.ccsDashboardAction;
    await action('antigravity-automatic','true'); assert.equal(calls.filter(c=>c.method==='PUT').length,0);
    await action('antigravity-pool','Gemini models (reported group)');
    await action('antigravity-automatic','true'); await action('antigravity-threshold','85%');
    assert.deepEqual(calls.filter(c=>c.method==='PUT').map(c=>({path:c.path,body:c.body})),[
      {path:'/api/antigravity/auto-switch',body:{requestedPoolId:'reported@gemini'}},
      {path:'/api/antigravity/auto-switch',body:{enabled:true}},
      {path:'/api/antigravity/auto-switch',body:{thresholdUsedPercent:85}},
    ]);
    await action('antigravity-activate','party'); assert.equal(offers.at(-1).product,'Antigravity'); assert.equal(offers.at(-1).targetProfile,'party@example.com'); assert.equal(JSON.stringify(offers).includes(token),false);
    const before=calls.length; await action('activate','other'); await action('antigravity-automatic','false'); assert.equal(calls.length,before);
    await action('activation-cancel',''); assert.equal(confirmationPosts,0);
    await action('antigravity-activate','party'); await action('activation-confirm',''); await action('activation-confirm','');
    assert.equal(confirmationPosts,1); assert.deepEqual(calls.find(c=>c.path.endsWith('/party/confirm')).body,{hostId:'ubuntu',confirmationToken:token});
    assert.equal(models.at(-1).antigravityAccounts.find(a=>a.profile==='party').selected,true); assert.equal(models.at(-1).antigravityAccounts.find(a=>a.profile==='party').runtimeVerified,true);
    assert.ok(calls.every(c=>c.credentials==='same-origin')); assert.equal(calls.some(c=>c.path.startsWith('/api/codex')&&c.method==='POST'),false);
  } finally { restore(); }
});
