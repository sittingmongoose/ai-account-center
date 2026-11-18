"""Sol pair-2 production-default old-to-modern reproduction; owned namespace/loopback only.
Run: sudo -n unshare --mount --fork python3 <this file> <empty scratch> <OpenSSH-8.1 ssh>
"""
import os,pathlib,shutil,socket,subprocess,sys,time,json
root=pathlib.Path(sys.argv[1]).resolve()
root.mkdir(exist_ok=True)
old_client=str(pathlib.Path(sys.argv[2]).resolve())
def run(argv,**kw):return subprocess.run(argv,check=True,capture_output=True,**kw)
server=None
try:
 for name in ['key','host']:run(['ssh-keygen','-q','-t','ed25519','-N','','-C','fixture-only','-f',str(root/name)])
 (root/'home/.ssh').mkdir(parents=True,mode=0o700);(root/'run/sshd').mkdir(parents=True)
 (root/'empty-config').write_text('')
 run(['mount','--make-rprivate','/'])
 for a,b in [('home','/root'),('run','/run'),('empty-config','/etc/ssh/ssh_config')]:run(['mount','--bind',str(root/a),b])
 with socket.socket() as s:s.bind(('127.0.0.1',0));port=s.getsockname()[1]
 pub=(root/'host.pub').read_text().split()
 (root/'alternate-trust').write_text(f'[127.0.0.1]:{port} {pub[0]} {pub[1]}\n')
 (root/'known_hosts').write_text('');(root/'authorized_keys').write_text((root/'key.pub').read_text())
 (root/'home/.ssh/config').write_text(f'Host *\n IgnoreUnknown KnownHostsCommand\n KnownHostsCommand /bin/cat {root}/alternate-trust\n')
 (root/'home/config.yaml').write_text(json.dumps({'version':10,'agentTransportSsh':{'knownHostsFile':str(root/'known_hosts')},'computers':{'fixture':{'sshAlias':'fixture','transport':{'host':'127.0.0.1','user':'root','port':port,'key':str(root/'key')}}}}))
 (root/'sshd_config').write_text(f'Port {port}\nListenAddress 127.0.0.1\nHostKey {root}/host\nPidFile {root}/sshd.pid\nAuthorizedKeysFile {root}/authorized_keys\nStrictModes no\nPermitRootLogin yes\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nUsePAM no\nLogLevel ERROR\n')
 server=subprocess.Popen(['/usr/sbin/sshd','-D','-e','-f',str(root/'sshd_config')],stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
 for _ in range(100):
  with socket.socket() as s:
   if s.connect_ex(('127.0.0.1',port))==0:break
  assert server.poll() is None
  time.sleep(.01)
 env={'PATH':os.environ['PATH'],'HOME':str(root/'home'),'CCS_HOME':str(root/'home'),'CCS_DIR':str(root/'home'),'AAC_REAL_FIXTURE_ROOT':str(root),'TMPDIR':str(root),'AAC_TEST_OPENSSH_81':old_client,'AAC_NO_LOCAL_RUST_BUILD':'1'}
 result=subprocess.run(['node',str(pathlib.Path(__file__).with_suffix('.cjs'))],env=env,timeout=30,text=True,capture_output=True)
 print(result.stdout);print(result.stderr,file=sys.stderr)
 assert result.returncode == 0, 'production default accepted a changed client'
finally:
 if server:
  server.terminate()
  try:server.wait(timeout=2)
  except subprocess.TimeoutExpired:server.kill();server.wait()
  server.stderr.close()
 shutil.rmtree(root)
