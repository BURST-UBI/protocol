"""First native GUI environment diagnostic, not GUI acceptance."""
import hashlib, json, os, pathlib, plistlib, signal, subprocess, tarfile, tempfile, time
root = pathlib.Path.cwd()
out = root / 'macos-gui-environment-evidence'
out.mkdir()
plan = json.loads(pathlib.Path('validation-inputs/gui-environment-plan.json').read_text())
subprocess.run(['node','tools/validation-reuse.mjs','begin','validation-inputs/gui-environment-plan.json',str(out/'start.json')],check=True)
def run(args, timeout=120):
    return subprocess.run(args, check=True, capture_output=True, text=True, timeout=timeout).stdout
def write(name, obj):
    (out / name).write_text(json.dumps(obj, indent=2) + '\n')
assert os.environ['GITHUB_RUN_ATTEMPT'] == '1'
with tempfile.TemporaryDirectory() as name:
    tmp = pathlib.Path(name)
    key = tmp / 'key.pem'
    key.write_text(os.environ.pop('EVIDENCE_DECRYPT_KEY'))
    key.chmod(0o600)
    archive = tmp / 'evidence.tar.gz'
    run(['openssl','cms','-decrypt','-binary','-inform','DER','-in','prior-encrypted/macos-evidence.cms','-inkey',str(key),'-out',str(archive)])
    with tarfile.open(archive) as tar:
        for member in tar.getmembers():
            p = pathlib.Path(member.name)
            assert not p.is_absolute() and '..' not in p.parts
            assert p.parts[0] == 'macos-signing-evidence' and (member.isfile() or member.isdir())
        tar.extractall(tmp, filter='data')
    prior = tmp / 'macos-signing-evidence'
    plan = json.loads((prior/'plan.json').read_text())
    compiler = run(['xcrun','--find','swiftc']).strip()
    run([compiler,'-j','1','tools/macos-gui-environment.swift','-o',str(out/'probe')], timeout=180)
    write('environment.json', {'runner':os.environ['RUNNER_OS'], 'architecture':os.environ['RUNNER_ARCH'], 'sourceRun':os.environ['PRIOR_RUN'], 'scope':'Desktop session and Accessibility availability only; no GUI acceptance', 'compiler':compiler})
    for app, expected in plan['apps'].items():
        result_run = os.environ['PRIOR_RUN'] if app == 'node-gui' else '40867797950'
        result = json.loads((prior/f'{app}.collection-{result_run}.result.json').read_text())
        assert result['outcome'] == 'pass'
        package = prior / result['archive']
        assert hashlib.sha256(package.read_bytes()).hexdigest() == result['sha256']
        install = pathlib.Path.home()/'Applications'/'BurstValidation'
        install.mkdir(parents=True, exist_ok=True)
        run(['ditto','-x','-k',str(package),str(install)])
        bundle = install / expected['bundleName']
        info = plistlib.loads((bundle/'Contents/Info.plist').read_bytes())
        assert info['CFBundleIdentifier'] == expected['bundleIdentifier']
        assert not (pathlib.Path.home()/'Library/Application Support'/expected['bundleIdentifier']).exists(), 'Profile must start fresh'
        run(['codesign','--verify','--deep','--strict',str(bundle)])
        run(['spctl','--assess','--type','execute',str(bundle)])
        binary = bundle/'Contents/MacOS'/info['CFBundleExecutable']
        with (out/f'{app}.launch.log').open('w') as log:
            process = subprocess.Popen([str(binary)], stdout=log, stderr=log, start_new_session=True)
            try:
                time.sleep(8)
                state = json.loads(run([str(out/'probe'),str(process.pid)]))
                state.update(archiveSha256=result['sha256'], processExit=process.poll())
                write(f'{app}.json', state)
                assert process.poll() is None, 'Application exited during preflight'
            finally:
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGTERM)
                    try: process.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        os.killpg(process.pid, signal.SIGKILL)
                        process.wait(timeout=5)

write('result.json', {'outcome':'pass','exitCode':0,'assertions':8,'review':'Both exact notarized archives verified, fresh installed profiles, strict signatures and Gatekeeper verified, both apps remained running. AX availability is diagnostic only, not GUI acceptance.','logs':[str(out/'wallet.json'),str(out/'node-gui.json'),str(out/'environment.json'),str(out/'wallet.launch.log'),str(out/'node-gui.launch.log')]})
run(['node','tools/validation-reuse.mjs','finish',str(out/'start.json'),str(out/'result.json'),str(out/'receipt.json')])
