"""Build frozen desktop inputs with separate receipts; never publish or sign."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tarfile
import time

root = Path(__file__).resolve().parent.parent
source = root / 'candidate-source'
desktop = root / 'desktop-source'
inputs = root / 'validation-inputs'
evidence = root / 'macos-desktop-evidence'
manifest = json.loads((inputs / 'desktop-source-manifest.json').read_text())
target = os.environ['MACOS_TARGET']
node = str(Path(shutil.which('node')).resolve())
checker = source / 'tools/validation-reuse.mjs'
cache = root / '.macos-target'
os.environ['CARGO_TARGET_DIR'] = str(cache)
os.environ['RUSTUP_TOOLCHAIN'] = 'nightly-2026-10-11'

def write(path, value):
    with path.open('x') as f:
        json.dump(value, f, indent=2)
        f.write('\n')

def query(command):
    return subprocess.check_output(command, text=True, stderr=subprocess.STDOUT, timeout=60).strip()

def receipt(*args):
    subprocess.run([node, str(checker), *map(str, args)], cwd=source, check=True, timeout=120)

def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

if sys.argv[1] == 'prepare':
    assert os.environ.get('GITHUB_RUN_ATTEMPT', '1') == '1', 'Audit latest evidence before any retry'
    evidence.mkdir(exist_ok=False)
    desktop.mkdir(exist_ok=False)
    assert digest(inputs / 'desktop-source.tar.gz') == manifest['archiveSha256']
    with tarfile.open(inputs / 'desktop-source.tar.gz') as archive:
        members = archive.getmembers()
        assert len(members) == len(manifest['files'])
        for member in members:
            assert member.isfile() and member.name in manifest['files']
            assert not Path(member.name).is_absolute() and '..' not in Path(member.name).parts
            assert not (desktop / member.name).exists(), 'Refuse to overwrite existing input'
        archive.extractall(desktop, filter='data')
    for name, expected in manifest['files'].items():
        assert digest(desktop / name) == expected, name
    (desktop / 'protocol').symlink_to(source, target_is_directory=True)
    assert (desktop / 'protocol').resolve() == source
    shutil.copy2(root / 'macos-candidate-evidence/host.json', evidence / 'host.json')
    write(evidence / 'prepare.json', {'outcome':'pass','verifiedFiles':len(manifest['files']),
        'protocolPath':str((desktop/'protocol').resolve()), 'scope':'Input identity only; no desktop runtime qualification'})
    raise SystemExit(0)

app, mode = sys.argv[1:]
assert app in ('wallet','node-gui')
appdir = desktop / app
assert json.loads((evidence/'prepare.json').read_text())['outcome'] == 'pass'
assert shutil.disk_usage(cache).free >= 8 * 1024**3, 'Less than8GiB target headroom'
key = app + '-' + mode
p = evidence / key
cargo = query(['rustup','which','cargo'])
rustc = query(['rustup','which','rustc'])
os.environ['RUSTC'] = rustc
rustlib = query([rustc,'--print','target-libdir','--target',target])
deps = [str(desktop/name) for name in manifest['files'] if name.startswith(app+'/')]
previous = None
cwd = appdir
product = None
if mode == 'dependencies':
    npm = str(Path(shutil.which('npm')).resolve())
    command = [node,npm,'ci','--no-audit','--no-fund']
    deps += [npm]
    scope = 'Locked npm dependencies for frozen frontend reconstruction; no functional acceptance'
elif mode == 'frontend':
    previous = app+'-dependencies'
    command = [node,str(appdir/'node_modules/vite/bin/vite.js'),'build']
    deps += sorted({str(f.resolve()) for f in (appdir/'node_modules').rglob('*') if f.is_file()})
    scope = 'Reconstruct and compare every previously qualified frontend output hash; no repeated browser/typecheck suite'
elif mode == 'native':
    previous = app+'-frontend'
    binary = 'burst-wallet-gui' if app == 'wallet' else 'burst-node-gui'
    command = [node,str(appdir/'node_modules/@tauri-apps/cli/tauri.js'),'build','--ci',
        '--no-bundle','--target',target,'--config',json.dumps({'build':{'beforeBuildCommand':''}}),'--','--locked']
    cwd = appdir
    deps += [str(source),str(appdir/'dist'),cargo,rustc,rustlib]
    deps += sorted({str(f.resolve()) for f in (appdir/'node_modules/@tauri-apps').rglob('*') if f.is_file()})
    product = cache / target / 'release' / binary
    scope = 'Frozen native Mach-O desktop release compilation only; no GUI/signing/notarization claim'
else:
    raise SystemExit('Unknown stage')
if previous:
    prior = evidence/previous
    receipt('check',prior.with_suffix('.plan.json'),prior.with_suffix('.receipt.json'))
    result = json.loads(prior.with_suffix('.result.json').read_text())
    assert result['outcome']=='pass' and result['exitCode']==0
plan = {'requirement':app+' native macOS '+mode,'scope':scope,'command':command,
 'workingDirectory':str(cwd),
 'inputs':deps+[node,str(Path(__file__).resolve()),str(checker),str(evidence/'host.json'),
    str(inputs/'desktop-source-manifest.json'),str(inputs/'desktop-run-plan.json')],
 'context':{'toolchain':'Pinned nightly2026-10-11 child Rust; Node22; captured host SDK',
 'features':'Unchanged application default features, release profile and exact frontend bytes',
 'network':'Dependency downloads only; no live deployment',
 'topology':target+' native approved GitHub-hosted VM; canonical protocol source path explicitly resolved',
 'limits':'One Cargo worker; approved whole-host envelope; each command1200s+10s cleanup; no Azure'},
 'changeImpact':json.loads((inputs/'desktop-run-plan.json').read_text())['changeImpact']}
write(p.with_suffix('.plan.json'),plan)
receipt('begin',p.with_suffix('.plan.json'),p.with_suffix('.start.json'))
started = time.monotonic()
code = 1
review = scope
logs = [str(p.with_suffix('.log'))]
try:
    with p.with_suffix('.log').open('x') as log:
        child = subprocess.Popen(['/usr/bin/nice','-n','10',*command],cwd=cwd,
            stdout=log,stderr=subprocess.STDOUT,start_new_session=True)
        try:
            code = child.wait(timeout=1200)
        except subprocess.TimeoutExpired:
            os.killpg(child.pid,signal.SIGTERM)
            try: child.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid,signal.SIGKILL)
                child.wait()
            try: os.killpg(child.pid,signal.SIGKILL)
            except ProcessLookupError: pass
            code = 124
    if code == 0 and mode == 'frontend':
        actual = {str(f.relative_to(desktop)):digest(f) for f in (appdir/'dist').rglob('*') if f.is_file()}
        expected = {k:v for k,v in manifest['expectedFrontend'].items() if k.startswith(app+'/')}
        comparison = {'actual':actual,'expected':expected,'equal':actual==expected}
        write(p.with_suffix('.comparison.json'),comparison)
        logs += [str(p.with_suffix('.comparison.json'))]
        assert actual==expected, 'Reconstructed frontend differs; preserve and diagnose before native compilation'
        shutil.copytree(appdir/'dist',evidence/(app+'-dist'))
        logs += [str(evidence/(app+'-dist'))]
    if code == 0 and product is not None:
        destination = evidence/product.name
        shutil.copy2(product,destination)
        info = query(['file',str(destination)])
        assert 'Mach-O 64-bit executable' in info
        assert ('arm64' if target.startswith('aarch64') else 'x86_64') in info
        write(p.with_suffix('.artifact.json'),{'sha256':digest(destination),'file':info,
            'linkage':query(['otool','-L',str(destination)])})
        logs += [str(destination),str(p.with_suffix('.artifact.json'))]
except Exception as error:
    code = code or 1
    review += '; '+type(error).__name__+': '+str(error)
finally:
    result = {'outcome':'pass' if code==0 else 'fail','exitCode':code,
        'assertions':1 if code==0 else 0,'elapsedSeconds':time.monotonic()-started,
        'review':review,'logs':logs}
    write(p.with_suffix('.result.json'),result)
    receipt('finish',p.with_suffix('.start.json'),p.with_suffix('.result.json'),p.with_suffix('.receipt.json'))
    receipt('check',p.with_suffix('.plan.json'),p.with_suffix('.receipt.json'))
print(json.dumps(result))
raise SystemExit(code)
