"""Guarded, explicit cleanup proposal: remove only the exact temporary hook.

Pure preparation and guarded reads have no mutation. apply_removal is separate;
its real native settings use requires standalone root authorization.
"""
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import stat
import tempfile

from native_status_attestor import bounded_json, StatusError

MAX_SETTINGS = 256 * 1024


class CleanupError(ValueError):
    pass


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


def stat_pin(info):
    return {'device':info.st_dev,'inode':info.st_ino,'uid':info.st_uid,'gid':info.st_gid,
        'mode':stat.S_IMODE(info.st_mode),'nlink':info.st_nlink,'size':info.st_size,
        'mtimeNs':info.st_mtime_ns,'ctimeNs':info.st_ctime_ns}


def guarded_read(path):
    path = Path(path)
    try:
        parent = path.parent.lstat()
        if (not stat.S_ISDIR(parent.st_mode) or parent.st_uid != os.getuid() or
                stat.S_IMODE(parent.st_mode) & 0o022 or path.parent.resolve() != path.parent):
            raise CleanupError('cleanup-parent-unsafe')
        before = path.lstat()
        if (not stat.S_ISREG(before.st_mode) or before.st_uid != os.getuid() or
                before.st_nlink != 1 or stat.S_IMODE(before.st_mode) != 0o600 or
                not 0 < before.st_size <= MAX_SETTINGS):
            raise CleanupError('cleanup-file-unsafe')
        descriptor = os.open(path,os.O_RDONLY | os.O_NOFOLLOW)
        with os.fdopen(descriptor,'rb') as stream:
            if stat_pin(os.fstat(stream.fileno())) != stat_pin(before):
                raise CleanupError('cleanup-file-changed')
            raw = stream.read(MAX_SETTINGS + 1)
            if (len(raw) > MAX_SETTINGS or stat_pin(os.fstat(stream.fileno())) != stat_pin(before)):
                raise CleanupError('cleanup-file-changed')
        if stat_pin(path.lstat()) != stat_pin(before):
            raise CleanupError('cleanup-file-changed')
        return stat_pin(before),raw
    except CleanupError:
        raise
    except OSError:
        raise CleanupError('cleanup-read-failed') from None


def parse(raw):
    try:
        return bounded_json(raw,MAX_SETTINGS)
    except (StatusError,UnicodeError):
        raise CleanupError('cleanup-json-invalid') from None


def exact(a,b):
    if type(a) is not type(b):return False
    if type(a) is dict:return set(a)==set(b) and all(exact(a[k],b[k]) for k in a)
    if type(a) is list:return len(a)==len(b) and all(exact(x,y) for x,y in zip(a,b))
    return a==b


def _members(text):
    """Top-level key/value/comma spans only; strict parser validates first."""
    decoder=json.JSONDecoder();position=0;members=[]
    def skip(pos):
        while pos<len(text) and text[pos] in ' \t\r\n':pos+=1
        return pos
    position=skip(position)+1;previous_comma=None
    while True:
        position=skip(position)
        if text[position:position+1]=='}':return members
        start=position;key,end=decoder.raw_decode(text,position)
        position=skip(end)+1;value_start=skip(position)
        _,end=decoder.raw_decode(text,value_start)
        position=skip(end);following=position if text[position:position+1]==',' else None
        members.append((key,start,value_start,end,previous_comma,following))
        if following is None:return members
        previous_comma=following;position=following+1


@dataclass(frozen=True)
class RemovalProposal:
    current:bytes
    replacement:bytes
    current_pin:dict


def prepare_removal(original,proposed,current,current_pin):
    old,temporary,now=parse(original),parse(proposed),parse(current)
    if 'statusLine' in old:raise CleanupError('cleanup-original-hook-present')
    if (not exact({k:v for k,v in temporary.items() if k!='statusLine'},old) or
            type(temporary.get('statusLine')) is not dict):
        raise CleanupError('cleanup-proposal-unrelated-change')
    if 'statusLine' not in now or not exact(now['statusLine'],temporary['statusLine']):
        raise CleanupError('cleanup-hook-mismatch')
    text=current.decode('utf-8');entries=[v for v in _members(text) if v[0]=='statusLine']
    if len(entries)!=1:raise CleanupError('cleanup-hook-span-invalid')
    _,start,_,end,previous,following=entries[0]
    if following is not None:end=following+1
    elif previous is not None:start=previous
    replacement=(text[:start]+text[end:]).encode('utf-8')
    remaining={k:v for k,v in now.items() if k!='statusLine'}
    if not exact(parse(replacement),remaining):raise CleanupError('cleanup-unrelated-change')
    return RemovalProposal(current,replacement,dict(current_pin))


def prepare_hook_reversal(original,proposed,current,current_pin):
    """Restore only the exact original hook value if a user hook preexisted.

    The no-original-hook branch is the unchanged actual12/13 lexical removal.
    All unrelated current values and bytes stay exactly as native/user left them.
    """
    old,temporary,now=parse(original),parse(proposed),parse(current)
    if 'statusLine' not in old:return prepare_removal(original,proposed,current,current_pin)
    if (not exact({k:v for k,v in temporary.items() if k!='statusLine'},
                  {k:v for k,v in old.items() if k!='statusLine'}) or
            'statusLine' not in now or not exact(now['statusLine'],temporary.get('statusLine'))):
        raise CleanupError('cleanup-hook-mismatch')
    old_text=original.decode('utf-8');text=current.decode('utf-8')
    old_spans=[entry for entry in _members(old_text) if entry[0]=='statusLine']
    spans=[entry for entry in _members(text) if entry[0]=='statusLine']
    if len(old_spans)!=1 or len(spans)!=1:raise CleanupError('cleanup-hook-span-invalid')
    start,end=spans[0][2:4];old_start,old_end=old_spans[0][2:4]
    replacement=(text[:start]+old_text[old_start:old_end]+text[end:]).encode()
    expected=dict(now);expected['statusLine']=old['statusLine']
    if not exact(parse(replacement),expected):raise CleanupError('cleanup-unrelated-change')
    return RemovalProposal(current,replacement,dict(current_pin))


def apply_removal(path,proposal,*,before_commit=lambda:None,audit=None):
    """Explicit fixture-reviewed write; no baseline/auth/history restoration.

    Compare-before-replace is not hostile same-user atomic compare-and-swap.
    Commit is recorded before directory fsync; never undo a successful replace.
    """
    path=Path(path);audit={} if audit is None else audit;audit['committed']=False
    pin,raw=guarded_read(path)
    if pin!=proposal.current_pin or raw!=proposal.current:
        raise CleanupError('cleanup-current-pin-conflict')
    descriptor,temporary=tempfile.mkstemp(prefix='.hook-cleanup-',dir=path.parent)
    temporary=Path(temporary)
    staged=os.fstat(descriptor);temporary_identity=(staged.st_dev,staged.st_ino)
    try:
        with os.fdopen(descriptor,'wb') as stream:
            os.fchmod(stream.fileno(),0o600)
            if os.fstat(stream.fileno()).st_gid!=pin['gid']:
                os.fchown(stream.fileno(),-1,pin['gid'])
            stream.write(proposal.replacement);stream.flush();os.fsync(stream.fileno())
            staged=os.fstat(stream.fileno());temporary_identity=(staged.st_dev,staged.st_ino)
        before_commit()  # Fresh no-native-writer census, injected by runner.
        latest,raw=guarded_read(path)
        if latest!=proposal.current_pin or raw!=proposal.current:
            raise CleanupError('cleanup-current-pin-conflict')
        os.replace(temporary,path)
        audit['committed']=True;audit['committedIdentity']=temporary_identity
        parent=os.open(path.parent,os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:os.fsync(parent)
        finally:os.close(parent)
        result,current=guarded_read(path)
        if ((result['device'],result['inode'])!=temporary_identity or current!=proposal.replacement):
            raise CleanupError('cleanup-postcommit-conflict')
        return result
    except CleanupError:
        raise
    except OSError:
        raise CleanupError('cleanup-write-committed-sync-failed' if audit['committed']
                           else 'cleanup-write-failed') from None
    finally:
        if temporary.exists() and not temporary.is_symlink():
            info=temporary.lstat()
            if (info.st_dev,info.st_ino)==temporary_identity:
                temporary.unlink()
