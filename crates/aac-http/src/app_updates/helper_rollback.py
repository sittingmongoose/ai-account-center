# Select the previous Update apps helper generation again. Run as the account that runs the
# agent: python3 helper_rollback.py (python helper_rollback.py on Windows). The generation it
# leaves becomes the new previous one, so running it twice returns to where it started.
import os, pathlib, re, sys, uuid

root = pathlib.Path.home() / '.ccs' / 'app-updates'
lines = (root / 'app_updates.py').read_text(encoding='utf-8').splitlines()
current = lines[0].removeprefix('# aac-helpers: ').strip() if lines else ''
previous = next((line.removeprefix('# aac-helpers-previous: ').strip() for line in lines
                 if line.startswith('# aac-helpers-previous: ')), '')
pattern = re.compile(r'helpers-[a-f0-9]{32}')
if not (pattern.fullmatch(current) and pattern.fullmatch(previous)
        and (root / previous / 'app_updates.py').is_file()):
    sys.exit('No previous helper generation to select.')
launcher = root / ('.launcher-' + uuid.uuid4().hex)
launcher.write_text(
    '# aac-helpers: ' + previous + '\n# aac-helpers-format: 2\n'
    + '# aac-helpers-previous: ' + current + '\n'
    + 'import pathlib, runpy, sys\n'
    + 'root = pathlib.Path(__file__).resolve().parent / ' + repr(previous) + '\n'
    + 'sys.path.insert(0, str(root))\n'
    + 'runpy.run_path(str(root / "app_updates.py"), run_name="__main__")\n', encoding='utf-8')
launcher.chmod(0o600)
os.replace(launcher, root / 'app_updates.py')
print('Selected ' + previous + '; ' + current + ' is kept as the previous generation.')
