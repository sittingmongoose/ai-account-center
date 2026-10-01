import test from 'node:test';
import assert from 'node:assert/strict';
class Element {
  children=[]; listeners={}; style={}; textContent=''; value='';
  append(...items){this.children.push(...items);}
  replaceChildren(...items){this.children=[...items];}
  addEventListener(kind,fn){this.listeners[kind]=fn;}
}
let serial=0;
const freshSample = () => ({provider:'qwen',platform:'windows',status:'ok',fetchedAt:new Date().toISOString(),sampledAt:new Date().toISOString(),windows:[{key:'monthly',label:'Monthly',usedPercent:125,used:1250,limit:1000,remaining:null,unit:'credits'},{key:'addon-pack-aaaaaaaaaaaa',label:'Additional credit pack 1',usedPercent:64.68614020609749,remaining:7062.7719587805,unit:'credits',expiresAt:'2026-10-08T16:00:00Z'}]});
async function popup(status,selectedRegion='intl') {
  const nodes=Object.fromEntries(['region','sync','status','windows'].map(id=>[id,new Element()]));
  let storageChanged;
  globalThis.document={getElementById:id=>nodes[id],createElement:()=>new Element()};
  globalThis.chrome={runtime:{async sendMessage(){return {region:selectedRegion,status};}},storage:{onChanged:{addListener(fn){storageChanged=fn;}}}};
  await import(`../extension/popup.mjs?fixture=${++serial}`);
  return {nodes,storageChanged};
}
const text = node => [node.textContent,...node.children.map(text)].join(' ');
test('transient failure keeps its error and shows clearly historical identity-unknown data',async () => {
  const sample=freshSample();
  const {nodes}=await popup({ok:false,region:'intl',errorCode:'network_error',previousSample:{sample,region:'intl',lastSuccessAt:sample.sampledAt,identity:'unknown'}});
  assert.match(nodes.status.textContent,/could not finish/);assert.doesNotMatch(nodes.status.textContent,/Updated/);
  const rendered=text(nodes.windows);
  assert.match(rendered,/Previous sample — account identity unknown/);
  assert.match(rendered,/International.*QwenCloud/);assert.match(rendered,/Sampled/);
  assert.match(rendered,/Historical usage/);assert.match(rendered,/current account has not been verified/);
  assert.match(rendered,/125% used/);assert.match(rendered.replaceAll(',',''),/7062\.7719587805 credits left/);
  const monthly=nodes.windows.children.find(node=>node.className==='window');
  assert.equal(monthly.children.at(-1).children[0].style.width,'100%');
});
test('historical data from another region cannot render',async () => {
  const sample=freshSample();
  const {nodes}=await popup({ok:false,errorCode:'network_error',previousSample:{sample,region:'intl',lastSuccessAt:sample.sampledAt,identity:'unknown'}},'cn');
  assert.equal(nodes.windows.children.length,0);
});
test('reopening after a region switch cannot label an old current sample Updated',async () => {
  const {nodes}=await popup({ok:true,region:'intl',lastSyncAt:new Date().toISOString(),sample:freshSample()},'cn');
  assert.equal(nodes.windows.children.length,0);assert.doesNotMatch(nodes.status.textContent,/Updated/);
  assert.match(nodes.status.textContent,/region or browser sign-in changed/);
});
test('expired historical data is withheld',async () => {
  const old=new Date(Date.now()-6*60*1000).toISOString();const sample={...freshSample(),fetchedAt:old,sampledAt:old};
  const {nodes}=await popup({ok:false,errorCode:'network_error',previousSample:{sample,region:'intl',lastSuccessAt:old,identity:'unknown'}});
  assert.equal(nodes.windows.children.length,0);
});
test('a sign-in source invalidation clears rows in an already open popup',async () => {
  const {nodes,storageChanged}=await popup({ok:true,region:'intl',lastSyncAt:new Date().toISOString(),sample:freshSample()});
  assert.equal(nodes.windows.children.length,2);
  storageChanged({status:{newValue:null}},'local');
  assert.equal(nodes.windows.children.length,0);assert.doesNotMatch(nodes.status.textContent,/Updated/);
});
test('historical rows expire within five minutes even while the popup remains open',async () => {
  const originalSetTimeout=globalThis.setTimeout, originalClearTimeout=globalThis.clearTimeout;
  let expire,delay;
  globalThis.setTimeout=(callback,milliseconds)=>{expire=callback;delay=milliseconds;return {unref(){}};};
  globalThis.clearTimeout=()=>{};
  try {
    const sample=freshSample();
    const {nodes}=await popup({ok:false,errorCode:'network_error',previousSample:{sample,region:'intl',lastSuccessAt:sample.sampledAt,identity:'unknown'}});
    assert.equal(nodes.windows.children.length,3);
    assert.ok(delay>0&&delay<=5*60*1000+1);
    expire();
    assert.equal(nodes.windows.children.length,0);
    assert.match(nodes.status.textContent,/could not finish/);
  } finally {globalThis.setTimeout=originalSetTimeout;globalThis.clearTimeout=originalClearTimeout;}
});
