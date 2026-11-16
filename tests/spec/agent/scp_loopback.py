#!/usr/bin/env python3
"""Owned loopback sshd only: production Rust Computer::scp against an empty or trusted AAC pin.
Run inside an owned mount namespace via sudo -n unshare. argv: <short fixture root> <test exe> <8.1 ssh>.
The client config's KnownHostsCommand and global trust accept the host; only the AAC pin may.
All keys, trust files and homes are fixtures; nothing outside the root is written.
"""
import json, os, shutil, signal, socket, subprocess, sys, time
from pathlib import Path

root, exe, old_ssh = Path(sys.argv[1]), sys.argv[2], sys.argv[3]
def run(args, **kw):
    return subprocess.run(args, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, **kw)
for name in ('key', 'host'):
    run(['ssh-keygen', '-q', '-t', 'ed25519', '-N', '', '-C', 'aac-fixture-only', '-f', str(root/name)])
(root/'home/.ssh').mkdir(parents=True, mode=0o700)
(root/'run/sshd').mkdir(parents=True)
(root/'old-bin').mkdir()
shutil.copyfile(old_ssh, root/'old-bin/ssh'); (root/'old-bin/ssh').chmod(0o755)
(root/'empty-config').write_text('')
(root/'payload').write_text('fixture payload\n')
(root/'remote').mkdir()
run(['mount', '--make-rprivate', '/'])
run(['mount', '--bind', str(root/'home'), '/root'])
run(['mount', '--bind', str(root/'run'), '/run'])
run(['mount', '--bind', str(root/'empty-config'), '/etc/ssh/ssh_config'])
with socket.socket() as sock:
    sock.bind(('127.0.0.1', 0)); port = sock.getsockname()[1]
pub = (root/'host.pub').read_text().split()
trust = f'[127.0.0.1]:{port} {pub[0]} {pub[1]}\n'
(root/'global-known-hosts').write_text(trust)
(root/'trusted-pin').write_text(trust)
(root/'empty-pin').write_text('')
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
Subsystem sftp internal-sftp
LogLevel ERROR
''')
# IgnoreUnknown lets OpenSSH 8.1 read the same config; a modern client honours KnownHostsCommand.
(root/'home/.ssh/config').write_text(f'''IgnoreUnknown KnownHostsCommand
Host *
    IdentityFile {root}/key
    UserKnownHostsFile {root}/global-known-hosts
    GlobalKnownHostsFile {root}/global-known-hosts
    KnownHostsCommand /bin/cat {root}/global-known-hosts
''')
server = subprocess.Popen(['/usr/sbin/sshd', '-D', '-e', '-f', str(root/'sshd_config')], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)

def stop_masters():
    # AAC's policy masters persist; stop only those under this fixture root.
    for sock in root.rglob('cm-*'):
        subprocess.run(['ssh', '-S', str(sock), '-O', 'exit', 'fixture'], capture_output=True, timeout=5)
    for proc in Path('/proc').iterdir():
        if not proc.name.isdigit() or int(proc.name) in (os.getpid(), server.pid): continue
        try:
            if str(root).encode() in (proc/'cmdline').read_bytes():
                os.kill(int(proc.name), signal.SIGTERM)
        except (OSError, ProcessLookupError):
            pass

def case(name, run_id, ssh_path, pin):
    dest = root/'remote'/name
    spec = {'root': str(root), 'port': port, 'pin': str(root/pin), 'run': str(root/run_id),
            'path': ssh_path, 'payload': str(root/'payload'), 'dest': str(dest)}
    env = {'PATH': ssh_path, 'HOME': '/root', 'LANG': 'C', 'AAC_SCP_LOOPBACK_CHILD': json.dumps(spec)}
    child = subprocess.run([exe, '--exact', 'process::scp_loopback::child', '--nocapture', '--test-threads=1'],
                           env=env, capture_output=True, timeout=60)
    # With --nocapture the line follows libtest's 'test ... ' prefix.
    lines = [l.split('AAC_SCP_RESULT ', 1)[1] for l in child.stdout.decode().splitlines() if 'AAC_SCP_RESULT ' in l]
    assert child.returncode == 0 and lines, (name, child.stdout.decode()[-2000:], child.stderr.decode()[-2000:])
    stop_masters()
    return json.loads(lines[-1]), dest.exists()

try:
    for _ in range(100):
        with socket.socket() as sock:
            if sock.connect_ex(('127.0.0.1', port)) == 0: break
        assert server.poll() is None, server.stderr.read().decode()
        time.sleep(.01)
    modern = '/usr/bin:/bin'
    old = f'{root}/old-bin:/usr/bin:/bin'
    result, copied = case('m-trusted', 'a', modern, 'trusted-pin')
    assert result['ok'] and result['code'] == 0 and copied, ('trusted pin must copy', result)
    assert (root/'remote/m-trusted').read_text() == 'fixture payload\n'
    result, copied = case('m-empty', 'b', modern, 'empty-pin')
    assert not copied and not (result['ok'] and result['code'] == 0), ('empty AAC pin copied (modern ssh)', result)
    # The reviewer's input: OpenSSH 8.1 probed first on PATH, the modern ssh also on PATH for scp.
    result, copied = case('o-empty', 'c', old, 'empty-pin')
    assert not copied and not (result['ok'] and result['code'] == 0), ('empty AAC pin copied through scp', result)
    result, copied = case('o-trusted', 'd', old, 'trusted-pin')
    print(json.dumps({'oldTrustedCopied': copied, 'oldTrusted': result}))
    print('PASS scp runs the probed ssh; the empty AAC pin refuses for modern and OpenSSH 8.1 probes')
finally:
    stop_masters()
    server.terminate()
    try: server.wait(timeout=2)
    except subprocess.TimeoutExpired: server.kill(); server.wait()
    if server.stderr: server.stderr.close()
    for child in root.iterdir():
        if child.is_dir(): shutil.rmtree(child)
        else: child.unlink()
