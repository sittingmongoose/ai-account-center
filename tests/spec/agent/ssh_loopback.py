#!/usr/bin/env python3
"""Owned loopback sshd only. Run inside an owned mount namespace via sudo -n unshare.
argv JSON is produced by the real Rust/TS policy. All keys/trust/home are fixtures.
"""
import json, os, socket, subprocess, sys, time, shutil
from pathlib import Path

root = Path(sys.argv[1])
argv = json.loads(sys.argv[2])
trusted_argv = argv.get('trusted') if isinstance(argv, dict) else None
argv = argv['empty'] if isinstance(argv, dict) else argv
master = None
owned_control_dirs = []
control_dirs = [root]
def run(args, **kw):
    return subprocess.run(args, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, **kw)
for name in ('key', 'host'):
    run(['ssh-keygen', '-q', '-t', 'ed25519', '-N', '', '-C', 'aac-fixture-only', '-f', str(root/name)])
(root/'home/.ssh').mkdir(parents=True, mode=0o700)
(root/'run/sshd').mkdir(parents=True)
(root/'empty-config').write_text('')
run(['mount', '--make-rprivate', '/'])
run(['mount', '--bind', str(root/'home'), '/root'])
run(['mount', '--bind', str(root/'run'), '/run'])
run(['mount', '--bind', str(root/'empty-config'), '/etc/ssh/ssh_config'])
with socket.socket() as sock:
    sock.bind(('127.0.0.1', int(sys.argv[3]) if len(sys.argv) > 3 else 0)); port = sock.getsockname()[1]
pub = (root/'host.pub').read_text().split()
trust = f'[127.0.0.1]:{port} {pub[0]} {pub[1]}\n'
(root/'global-known-hosts').write_text(trust)
(root/'known_hosts').write_text('')
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
AcceptEnv KIMI_API_KEY AAC_FIXTURE_SETENV
AllowAgentForwarding yes
LogLevel ERROR
''')
agent = subprocess.Popen(['ssh-agent', '-D', '-a', str(root/'agent.sock')], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
server = subprocess.Popen(['/usr/sbin/sshd', '-D', '-e', '-f', str(root/'sshd_config')], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
try:
    for _ in range(100):
        if (root/'agent.sock').exists(): break
        time.sleep(.01)
    run(['ssh-add', str(root/'key')], env={'PATH':os.defpath,'SSH_AUTH_SOCK':str(root/'agent.sock')})
    (root/'home/.ssh/config').write_text(f'''Host *
    IdentityFile {root}/key
    UserKnownHostsFile {root}/known_hosts
    ForwardAgent {root}/agent.sock
    SendEnv KIMI_API_KEY
    SetEnv AAC_FIXTURE_SETENV=leaked KIMI_API_KEY=fixture-inline-provider-key
    GlobalKnownHostsFile {root}/global-known-hosts
    KnownHostsCommand /bin/cat {root}/global-known-hosts
    PermitLocalCommand yes
    LocalCommand touch {root}/local-command-ran
''')
    for _ in range(100):
        with socket.socket() as sock:
            if sock.connect_ex(('127.0.0.1', port)) == 0: break
        assert server.poll() is None, server.stderr.read().decode()
        time.sleep(.01)
    env = {'PATH': os.defpath, 'HOME':str(root/'home'), 'LANG':'C'}
    base = ['-p', str(port), '-l', 'root', '--', '127.0.0.1']
    # An existing user master must not bypass AAC's empty pin. Rust supplies its
    # own policy-bound master argv; TS is challenged with the user's socket path.
    (root/'known_hosts').write_text(trust)
    if trusted_argv is not None:
        for value in trusted_argv:
            if value.startswith('ControlPath='):
                control_dir = Path(value.split('=', 1)[1].strip('"')).parent
                if not control_dir.exists():
                    control_dir.mkdir(mode=0o700, parents=True)
                    if not control_dir.is_relative_to(root): owned_control_dirs.append(control_dir)
                control_dirs.append(control_dir)
        master_args = ['-o', 'ControlPersist=no', *trusted_argv]
    else:
        master_args = ['-M', '-S', str(root/'user-master.sock'), '-o', 'PermitLocalCommand=no', '-o', 'ForwardAgent=no']
        config = root/'home/.ssh/config'
        config.write_text(config.read_text() + f'    ControlMaster auto\n    ControlPath {root}/user-master.sock\n')
    master = subprocess.Popen(['ssh', '-N', *master_args, *base], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    control_ready = False
    for _ in range(100):
        if any(p.is_socket() and p.name != 'agent.sock' for directory in control_dirs for p in directory.rglob('*')):
            control_ready = True
            break
        assert master.poll() is None, master.stderr.read().decode()
        time.sleep(.01)
    assert control_ready, 'owned fixture master did not create its control socket'
    (root/'known_hosts').write_text('')
    command = ['ssh', '-o', 'ControlPersist=no', *argv, *base,
               'printf "key=%s set=%s agent=%s\\n" "$KIMI_API_KEY" "$AAC_FIXTURE_SETENV" "$SSH_AUTH_SOCK"']
    rejected = subprocess.run(command, env=env, capture_output=True, timeout=8)
    assert rejected.returncode != 0, ('empty AAC pin accepted alternate trust or existing master', rejected.stdout)
    assert b'Host key verification failed' in rejected.stderr, rejected.stderr
    (root/'known_hosts').write_text(trust)
    accepted = run(command, env=env, timeout=8)
    assert accepted.stdout == b'key= set= agent=\n', accepted.stdout
    assert not (root/'local-command-ran').exists(), 'inherited LocalCommand ran'
    print('PASS empty AAC pin refuses alternate trust; pinned connection sends no provider env or agent')
finally:
    for child in (master, server, agent):
        if child is None: continue
        child.terminate()
        try: child.wait(timeout=2)
        except subprocess.TimeoutExpired: child.kill(); child.wait()
    if server.stderr: server.stderr.close()
    if master and master.stderr: master.stderr.close()
    for directory in owned_control_dirs: shutil.rmtree(directory)
    for child in root.iterdir():
        if child.is_dir(): shutil.rmtree(child)
        else: child.unlink()
