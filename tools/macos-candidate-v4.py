"""Run existing candidate checks on native GitHub macOS hosts; retain stage receipts."""
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import signal
import subprocess
import sys
import tarfile
import time
from macos_process_grace import stop_group

workspace = Path(__file__).resolve().parent.parent
source = workspace / 'candidate-source'
evidence = workspace / 'macos-candidate-v4-evidence'
prior_evidence = workspace / 'macos-candidate-v3-evidence'
inputs = workspace / 'validation-inputs'
target = os.environ['MACOS_TARGET']
cache = workspace / '.macos-target'
os.environ['CARGO_TARGET_DIR'] = str(cache)
checker = workspace / 'tools/validation-reuse.mjs'
node = str(Path(shutil.which('node')).resolve())
mode = sys.argv[1]

def write(path, value):
    with path.open('x') as f:
        json.dump(value, f, indent=2)
        f.write('\n')

def checked(command, cwd=None):
    return subprocess.check_output(command, cwd=cwd, stderr=subprocess.STDOUT, text=True, timeout=60)

def receipt(*args):
    output = checked([node, str(checker), *map(str, args)], source)
    print(output, end='')

if mode == 'prepare':
    if os.environ.get('GITHUB_RUN_ATTEMPT', '1') != '1':
        raise SystemExit('Do not rerun the workflow unchanged; audit latest stage evidence first.')
    evidence.mkdir(exist_ok=False)
    assert platform.system() == 'Darwin'
    assert platform.machine() == ('arm64' if target.startswith('aarch64') else 'x86_64')
    manifest = json.loads((inputs / 'source-manifest.json').read_text())
    assert hashlib.sha256((inputs / 'source.tar.gz').read_bytes()).hexdigest() == manifest['archiveSha256']
    source.mkdir(exist_ok=False)
    with tarfile.open(inputs / 'source.tar.gz') as archive:
        members = archive.getmembers()
        assert len(members) == len(manifest['files'])
        for member in members:
            assert member.isfile() and member.name in manifest['files']
            assert not Path(member.name).is_absolute() and '..' not in Path(member.name).parts
        archive.extractall(source, filter='data')
    for name, digest in manifest['files'].items():
        assert hashlib.sha256((source / name).read_bytes()).hexdigest() == digest, name
    cache.mkdir(exist_ok=True)
    assert shutil.disk_usage(cache).free >= 8 * 1024**3, 'Less than8GiB target headroom'
    host = {name: checked(command) for name, command in {
        'os': ['sw_vers'], 'hardware': ['sysctl', 'hw.ncpu', 'hw.memsize'],
        'swap': ['sysctl', 'vm.swapusage'], 'sdk': ['xcrun', '--show-sdk-version'],
        'rust': ['rustc', '-vV'], 'cargo': ['cargo', '-V'], 'node': [node, '--version'],
    }.items()}
    write(evidence / 'host.json', host)
    write(evidence / 'prepare-result.json', {'outcome':'pass','filesVerified':len(manifest['files']),
        'archiveSha256':manifest['archiveSha256'],'scope':'Source and environment preflight only'})
    print('PASS source/host preflight:', len(manifest['files']), 'frozen files')
    raise SystemExit(0)

assert json.loads((evidence / 'prepare-result.json').read_text())['outcome'] == 'pass'
if mode == 'cleanup-preflight':
    child = subprocess.Popen(['/bin/sh','-c','sleep 30 & wait'], start_new_session=True)
    started = time.monotonic()
    outcome = 'fail'
    try:
        time.sleep(0.2)
        stop_group(child)
        outcome = 'pass'
    finally:
        write(evidence/'cleanup-preflight.json', {'outcome':outcome,'elapsedSeconds':time.monotonic()-started,
            'scope':'Disposable native parent and child group only; historical compiler cleanup not reproduced'})
    raise SystemExit(0)
if mode == 'daemon':
    assert (source/'vendor/lmdb-master-sys/lmdb').stat().st_mtime == 0
    assert json.loads((evidence/'cleanup-preflight.json').read_text())['outcome']=='pass'
cargo = checked(['rustup','which','cargo'], source).strip()
rustc = checked(['rustup','which','rustc'], source).strip()
os.environ['RUSTC'] = rustc
os.environ['RUSTUP_TOOLCHAIN'] = 'nightly-2026-10-11'
rustlib = checked([rustc, '--print','target-libdir','--target',target], source).strip()
base = [cargo,'build','--locked','--release','--target',target,'--timings','-vv']
if mode == 'daemon':
    command = base + ['-p','burst-daemon','--bin','burst-daemon','--features','private-testnet']
    product = cache / target / 'release/burst-daemon'
    dependencies = [str(source), cargo, rustc, rustlib]
    scope = 'Frozen native macOS daemon release compilation; not runtime, Developer ID or public acceptance'
elif mode == 'helper':
    prior = evidence / 'daemon-corrected'
    receipt('check', prior.with_suffix('.plan.json'), prior.with_suffix('.receipt.json'))
    assert json.loads(prior.with_suffix('.result.json').read_text())['outcome'] == 'pass'
    command = base + ['-p','burst-wallet-core','--example','validate_restart_payment']
    product = cache / target / 'release/examples/validate_restart_payment'
    dependencies = [str(source), cargo, rustc, rustlib]
    scope = 'Existing native epoch/restart helper compilation; reuse daemon target cache; not runtime'
elif mode == 'runtime':
    stages = []
    for name in ['daemon-corrected','helper']:
        p = evidence / name
        stages.append({'name':name,'plan':str(p.with_suffix('.plan.json')),'receipt':str(p.with_suffix('.receipt.json'))})
    write(evidence / 'runtime-stages.json', stages)
    receipt('audit', evidence / 'runtime-stages.json')
    daemon = evidence / 'burst-daemon'
    helper = evidence / 'validate_restart_payment'
    os.environ['BURST_EPOCH_VALIDATION_HELPER'] = str(helper)
    command = [node,str(source/'tools/daemon-smoke.mjs'),str(daemon),'--public-testnet-identity',str(inputs/'public-observation.json')]
    product = None
    dependencies = [str(daemon),str(helper),str(source/'tools/daemon-smoke.mjs'),str(inputs/'public-observation.json')]
    scope = 'Existing public identity/authority isolation/network separation and Dev epoch/owner successor through two history-preserving restarts; not GUI, signing, load or physical power-loss'
else:
    raise SystemExit('Unknown stage')

p = evidence / ('daemon-corrected' if mode == 'daemon' else mode)
plan = {'requirement': 'Native macOS '+target+' '+mode, 'scope':scope, 'command':command,
    'inputs':dependencies+[str(Path(__file__).resolve()),str(workspace/'tools/macos_process.py'),str(workspace/'tools/macos_process_grace.py'),str(workspace/'tools/macos-source-directory-metadata.py'),str(checker),node,str(evidence/'host.json'),str(inputs/'intel-daemon-cache-correction-plan.json')],
    'context':{'toolchain':'nightly2026-10-11; explicit RUSTUP_TOOLCHAIN and RUSTC; Node22; host-current.json records SDK/toolchain',
        'features':'private-testnet daemon with frozen public creator key; helper default features',
        'network':'Build dependency downloads or isolated disposable Dev/Test/Live; no public network deployment',
        'topology':'Native '+target+' GitHub-hosted VM; separate architecture hosts run in parallel',
        'limits':'User-approved hosted VM bounds; one Cargo worker; command1200s cleanup10s; no Azure'},
    'changeImpact':json.loads((inputs/'intel-daemon-cache-correction-plan.json').read_text())['changeImpact']}
if mode == 'daemon':
    assert target == 'x86_64-apple-darwin', 'Do not rerun passing ARM qualification'
    latest = json.loads((prior_evidence/'daemon-final.invalidated-receipt.json').read_text())
    reused_node = cache / target / 'release/build/burst-node/2bcae47e8e580fc8/out/libburst_node-2bcae47e8e580fc8.rlib'
    expected = next(row for row in latest['invalidation']['observedInputs'] if row[0] == str(reused_node))
    assert hashlib.sha256(reused_node.read_bytes()).hexdigest() == expected[2]
    observation = evidence/'cache-observation.json'
    write(observation, {'nodeSha256':expected[2], 'deploymentTarget':os.environ.get('MACOSX_DEPLOYMENT_TARGET'),
        'scope':'Cache observation only. Mutable output is not an immutable compiler input.'})
    assert os.environ.get('MACOSX_DEPLOYMENT_TARGET') is None
    plan['inputs'] += [str(observation), str(evidence/'cleanup-preflight.json')]
    plan['rerun']={'reason':'Correct measured cache invalidations: restore daemon-v3 cache without desktop deployment-setting changes and restore directory timestamps only after exact1093-file source verification. Latest invalidated failure retained; whole-group cleanup grace corrected. One bounded completion attempt, no ARM rerun.', 'attemptBudget':1}
write(p.with_suffix('.plan.json'), plan)
if mode == 'daemon':
    prior = prior_evidence / 'daemon-final'
    stages_path = evidence / 'prior-stages.json'
    write(stages_path,[{'name':'previous-daemon','plan':str(prior.with_suffix('.plan.json')),'receipt':str(prior.with_suffix('.invalidated-receipt.json'))}])
    audit = subprocess.run([node,str(checker),'audit',str(stages_path)],cwd=source,text=True,capture_output=True)
    (evidence/'prior-audit.json').write_text(audit.stdout)
    decisions = json.loads(audit.stdout)
    assert len(decisions)==1 and decisions[0]['status']=='unverified' and 'invalidated attempt' in decisions[0]['reason'], 'Prior evidence needs review; do not rebuild'
    assert json.loads(prior.with_suffix('.result.json').read_text())['exitCode']==124
    receipt('prepare',p.with_suffix('.plan.json'),prior.with_suffix('.invalidated-receipt.json'),p.with_suffix('.start.json'))
else:
    receipt('begin',p.with_suffix('.plan.json'),p.with_suffix('.start.json'))
start = time.monotonic()
code = 1
review = scope
logs = [str(p.with_suffix('.log'))]
try:
    with p.with_suffix('.log').open('x') as log:
        child = subprocess.Popen(['/usr/bin/nice','-n','10',*command], cwd=source,
            stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        try:
            code = child.wait(timeout=1200)
        except subprocess.TimeoutExpired:
            code = 124
            stop_group(child)
            review += '; command exceeded1200s; no pass'
    if code == 0 and product is not None:
        destination = evidence / product.name
        shutil.copy2(product, destination)
        binary_info = checked(['file', str(destination)])
        assert 'Mach-O 64-bit executable' in binary_info
        assert ('arm64' if target.startswith('aarch64') else 'x86_64') in binary_info
        write(p.with_suffix('.artifact.json'), {'sha256':hashlib.sha256(destination.read_bytes()).hexdigest(),
            'file':binary_info,'linkage':checked(['otool','-L',str(destination)])})
        logs += [str(destination),str(p.with_suffix('.artifact.json'))]
except Exception as error:
    code = code if code else 1
    review += '; '+type(error).__name__+': '+str(error)
    raise
finally:
    if mode in ('daemon','helper'):
        timings = cache/'cargo-timings'
        if timings.is_dir():
            saved=evidence/(mode+'-cargo-timings');shutil.copytree(timings,saved);logs.append(str(saved))
    result = {'outcome':'pass' if code==0 else 'fail','exitCode':code,'assertions':1 if code==0 else 0,
        'elapsedSeconds':time.monotonic()-start,'review':review,'logs':logs}
    write(p.with_suffix('.result.json'),result)
    receipt('finish',p.with_suffix('.start.json'),p.with_suffix('.result.json'),p.with_suffix('.receipt.json'))
    receipt('check',p.with_suffix('.plan.json'),p.with_suffix('.receipt.json'))
print(json.dumps(result))
raise SystemExit(code)
