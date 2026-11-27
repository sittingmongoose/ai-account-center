#!/usr/bin/env python3
"""Owned mount namespace and loopback sshd; never opens real client keys/home."""
import os, pathlib, shutil, socket, subprocess, sys, time
root, bun, builder, kind = pathlib.Path(sys.argv[1]), *sys.argv[2:]
def run(args, **kw):
    return subprocess.run(args, check=True, capture_output=True, **kw)
server = None
try:
    for name in ('key', 'wrong', 'host'):
        run(['ssh-keygen', '-q', '-t', 'ed25519', '-N', '', '-f', str(root/name)])
    (root/'home/.ssh').mkdir(parents=True, mode=0o700)
    (root/'run/sshd').mkdir(parents=True)
    (root/'empty-config').write_text('')
    run(['mount', '--make-rprivate', '/'])
    for source, target in [('home', '/root'), ('run', '/run'), ('empty-config', '/etc/ssh/ssh_config')]:
        run(['mount', '--bind', str(root/source), target])
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0)); port = sock.getsockname()[1]
    pub = (root/'host.pub').read_text().split()
    (root/'known_hosts').write_text(f'[127.0.0.1]:{port} {pub[0]} {pub[1]}\n')
    (root/'authorized_keys').write_text((root/'key.pub').read_text())
    (root/'sshd_config').write_text(f'''Port {port}
ListenAddress 127.0.0.1
HostKey {root}/host
PidFile {root}/sshd.pid
AuthorizedKeysFile {root}/authorized_keys
StrictModes no
PermitRootLogin yes
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
LogLevel ERROR
''')
    (root/'home/.ssh/config').write_text(f'''Host fixture
 HostName 127.0.0.1
 User root
 Port {port}
 IdentityFile {root}/missing
 IdentityFile {root}/wrong
 IdentityFile {root}/key
''' + ('Match exec "sleep 3"\n' if kind.endswith('slow') else ''))
    import json
    (root/'home/config.yaml').write_text(json.dumps({'version': 10, 'agentTransportSsh': {'knownHostsFile': str(root/'known_hosts')}}))
    server = subprocess.Popen(['/usr/sbin/sshd', '-D', '-e', '-f', str(root/'sshd_config')], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    for _ in range(100):
        with socket.socket() as sock:
            if sock.connect_ex(('127.0.0.1', port)) == 0: break
        assert server.poll() is None, server.stderr.read().decode()
        time.sleep(.01)
    env = {'PATH': os.environ['PATH'], 'HOME': str(root/'home'), 'CCS_HOME': str(root/'home'), 'CCS_DIR': str(root/'home'), 'AAC_NO_LOCAL_RUST_BUILD': '1', 'AAC_TEST_PORT': str(port), 'AAC_AGENT_KNOWN_HOSTS_FILE': str(root/'known_hosts')}
    p = subprocess.run([bun, builder], env=env, capture_output=True, text=True, timeout=20)
    print(p.stdout); print(p.stderr, file=sys.stderr)
    sys.exit(p.returncode)
finally:
    if server:
        server.terminate()
        try: server.wait(timeout=2)
        except subprocess.TimeoutExpired: server.kill(); server.wait()
        server.stderr.close()
    for child in root.iterdir():
        if child.is_dir(): shutil.rmtree(child)
        else: child.unlink()
