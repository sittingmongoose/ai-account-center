import {test, expect, spyOn, afterEach, mock} from 'bun:test';
import fs from 'fs';
import path from 'path';
import {PassThrough} from 'stream';
import * as childProcess from 'child_process';
import {runClaudeHistoryHelper} from '../../../src/web-server/services/claude-desktop-transport';
const { MAC_PROFILE } = require('./synthetic-history-fixtures.cjs');
const scripts = path.resolve(import.meta.dir, '../../../scripts/claude-history');
const mac = {launcherName:'Synthetic.app',sshHost:'synthetic-mac'};
const windows = {launcherName:'Synthetic.lnk',sshHost:'synthetic-windows'};
const request = (platform = 'mac',mode = 'collect') => ({mode,profileId:MAC_PROFILE,platform,policy:{synthetic:true},expectedEmail:'synthetic@example.com'});
function fakeSsh(error?: Error, output=Buffer.from('{"closed":true}')) {
  let packet:any, captured:any[]=[];
  const spy=spyOn(childProcess,'execFile').mockImplementation((...args:any[]) => {
    captured=args;
    const input=new PassThrough(); const chunks:Buffer[]=[];
    input.on('data', chunk => chunks.push(Buffer.from(chunk)));
    input.on('end',()=>{packet=JSON.parse(Buffer.concat(chunks).toString()); args.at(-1)(error??null,output,Buffer.from('SYNTHETIC_PRIVATE_STDERR'));});
    return {stdin:input} as any;
  });
  return {spy,packet:()=>packet,captured:()=>captured};
}
afterEach(()=>mock.restore());
test('collect uses fixed SSH and private RAM packet, never code/data in the command',async()=>{
  const f=fakeSsh(); expect(await runClaudeHistoryHelper(mac,'mac',MAC_PROFILE,request())).toEqual(Buffer.from('{"closed":true}'));
  const [program,args,options]=f.captured(); expect(program).toBe('ssh'); expect(args).toContain('synthetic-mac');
  expect(args.at(-1)).toContain('/usr/bin/python3 -c'); expect(args.at(-1)).not.toContain('synthetic@example.com');
  expect(args.at(-1).length).toBeLessThan(1000); expect(options).toMatchObject({encoding:'buffer',timeout:30000,maxBuffer:24*1024*1024});
  expect(f.packet().request).toEqual(request()); expect(f.packet().helperSource).toBe(fs.readFileSync(path.join(scripts,'history_index_remote_v2.py'),'utf8'));
  expect(f.packet()).not.toHaveProperty('nodeBridgeSource'); expect(f.spy).toHaveBeenCalledTimes(1);
});
test('Windows bootstrap stays short and interpreter path is fixed independent of request fields',async()=>{
  const f=fakeSsh(); await runClaudeHistoryHelper(windows,'windows',MAC_PROFILE,{...request('windows'),executable:'SYNTHETIC_INJECTED_EXECUTABLE'});
  const command=f.captured()[1].at(-1); const encoded=command.split(' ').at(-1); const script=Buffer.from(encoded,'base64').toString('utf16le');
  expect(command.length).toBeLessThan(3000); expect(script).toContain("'.ccs', 'claude-session-migration', 'venv', 'Scripts', 'python.exe'");
  expect(script).not.toContain('SYNTHETIC_INJECTED_EXECUTABLE'); expect(script).not.toContain('synthetic@example.com'); expect(f.spy).toHaveBeenCalledTimes(1);
});
test('append carries only the exact packaged writer and bridge in the private packet',async()=>{
  const f=fakeSsh(); await runClaudeHistoryHelper(mac,'mac',MAC_PROFILE,request('mac','append'));
  expect(f.packet().request.transactionSource).toBe(fs.readFileSync(path.join(scripts,'history_index_transaction_v1.cjs'),'utf8'));
  expect(f.packet().nodeBridgeSource).toBe(fs.readFileSync(path.join(scripts,'history_index_node_bridge_v2.cjs'),'utf8'));
  expect(f.captured()[1].at(-1)).not.toContain('transactionSource');
});
test('writer pin mismatch aborts before SSH and does not repair the package',async()=>{
  const filename=path.join(scripts,'history_index_transaction_v1.cjs'); const bytes=fs.readFileSync(filename); const f=fakeSsh();
  try {fs.writeFileSync(filename,Buffer.concat([bytes,Buffer.from('\n// SYNTHETIC_CHANGE\n')])); await expect(runClaudeHistoryHelper(mac,'mac',MAC_PROFILE,request('mac','append'))).rejects.toThrow('unavailable'); expect(f.spy).not.toHaveBeenCalled();}
  finally{fs.writeFileSync(filename,bytes);}
});
for(const [profile,mode,target] of [['unknown','collect','mac'],[MAC_PROFILE,'arbitrary-execute','mac'],[MAC_PROFILE,'collect','windows']] as const) test('invalid fixed binding cannot reach SSH',async()=>{
  const f=fakeSsh(); await expect(runClaudeHistoryHelper(mac,'mac',profile,request(target,mode))).rejects.toThrow(); expect(f.spy).not.toHaveBeenCalled();
});
test('transport error body and stderr never escape fixed public failure',async()=>{
  const f=fakeSsh(new Error('SYNTHETIC_PRIVATE_EXCEPTION'));
  try {await runClaudeHistoryHelper(mac,'mac',MAC_PROFILE,request()); throw new Error('expected refusal');}
  catch(error) {expect(String(error)).toBe('NetworkError: Claude desktop request failed.'); expect(String(error)).not.toContain('SYNTHETIC_PRIVATE');}
  expect(f.spy).toHaveBeenCalledTimes(1);
});
test('oversized private data is refused before SSH without a new file',async()=>{
  const f=fakeSsh(); await expect(runClaudeHistoryHelper(mac,'mac',MAC_PROFILE,{...request(),records:'x'.repeat(24*1024*1024)})).rejects.toThrow('unavailable'); expect(f.spy).not.toHaveBeenCalled();
});
