"""Native Windows guard vectors from the frozen Python helper, using offline OS doubles."""
import copy, importlib.util, io, json, pathlib, struct
from types import SimpleNamespace
from unittest.mock import patch
ROOT = pathlib.Path(__file__).resolve().parents[3]
spec = importlib.util.spec_from_file_location('history_oracle', ROOT/'scripts/claude-history/history_index_remote_v2.py')
h = importlib.util.module_from_spec(spec)
spec.loader.exec_module(h)
sha = h.NATIVE['windows'][2]
pkg = [{'Version':'2.19675.0.0','PackageFullName':'Claude_2.19675.0.0_x64__pzs8sxrjxfjjc','InstallLocation':r'C:\Program Files\WindowsApps\Claude_2.19675.0.0_x64__pzs8sxrjxfjjc'}]
header = json.dumps({'files':{'.vite':{'files':{'build':{'files':{'index.chunk-DmADLdv3.js':{'size':1,'offset':'0'}}}}}}}).encode()
archive = struct.pack('<4I',0,len(header),0,len(header)) + header + b'x'
class FakePath:
    def __init__(self,*args): pass
    def __truediv__(self,other): return self
    @property
    def parent(self): return self
    def lstat(self): return SimpleNamespace(st_mode=0o100600,st_size=len(archive),st_dev=1,st_ino=1,st_mtime_ns=1)
    def open(self,*args): return io.BytesIO(archive)
cases=[]
variants=[(pkg,sha),(pkg,'0'*64),([],sha),(pkg+pkg,sha)]
for field in ['Version','PackageFullName','InstallLocation']:
    bad=copy.deepcopy(pkg);bad[0][field]='wrong';variants.append((bad,sha))
for packages,digest in variants:
    with patch.object(h,'Path',FakePath),patch.object(h,'windows_powershell',lambda script:json.dumps(packages)),patch.object(h,'directory_identity',lambda p:None),patch.object(h,'sha',lambda raw:digest):
        try: expected=h.native_guard('windows');success=True
        except h.Refused: expected=None;success=False
    cases.append({'packages':packages,'sha':digest,'success':success,'expected':expected})
(ROOT/'crates/aac-agent/tests/fixtures/windows-native-guard.json').write_text(json.dumps(cases,indent=2)+'\n')
root=r'C:\profiles\work'
identities={root:'target',r'C:\profiles\alias':'target',r'C:\profiles\other':'other'}
rows=[[],[['Claude.exe']],[['Claude.exe','--user-data-dir='+root]],[['Claude.exe','--user-data-dir',r'C:\profiles\other']],[['Claude.exe','--user-data-dir',r'C:\profiles\alias']],[['Claude.exe','--user-data-dir']],[['Claude.exe','--user-data-dir=relative']],[['Claude.exe','--user-data-dir='+root,'--user-data-dir='+root]]]
process=[]
for profile in ['gmail','platyr']:
    for argv in rows:
        raw=[{'CommandLine':json.dumps(a)} for a in argv]
        def identity(p):
            if str(p) not in identities: raise h.Refused('unknown')
            return identities[str(p)]
        with patch.object(h,'Path',pathlib.PureWindowsPath),patch.object(h,'windows_powershell',lambda script:json.dumps(raw)),patch.object(h,'windows_arguments',lambda command:json.loads(command)),patch.object(h,'directory_identity',identity):
            try: expected=h.local_closed('windows',profile,pathlib.PureWindowsPath(root));success=True
            except h.Refused:expected=None;success=False
        process.append({'profile':profile,'root':root,'identities':identities,'argv':argv,'success':success,'expected':expected})
(ROOT/'crates/aac-agent/tests/fixtures/windows-processes.json').write_text(json.dumps(process,indent=2)+'\n')
print(len(cases),'native guard and',len(process),'process oracle cases')
