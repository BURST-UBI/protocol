"""First native GUI environment diagnostic, not GUI acceptance."""
import atexit, contextlib, hashlib, json, os, urllib.request, socket, pathlib, plistlib, signal, subprocess, tarfile, tempfile, time
root = pathlib.Path.cwd()
out = root / 'macos-gui-environment-evidence'
out.mkdir()
plan = json.loads(pathlib.Path('validation-inputs/gui-environment-plan.json').read_text())
subprocess.run(['python3','tools/macos-gui-prior-receipt.py'],check=True)
subprocess.run(['node','tools/validation-reuse.mjs','prepare','validation-inputs/gui-environment-plan.json','prior-gui/receipt.json',str(out/'start.json')],check=True)
prior_logs=out/'previous-stage';prior_logs.mkdir()
for path, *_ in json.loads(pathlib.Path('prior-gui/receipt.json').read_text())['evidence']:
    old=pathlib.Path(path);old.rename(prior_logs/old.name)
def preserve_failure():
    if (out/'start.json').exists() and not (out/'receipt.json').exists():
        result={'outcome':'fail','exitCode':1,'assertions':0,'review':'Preflight did not reach terminal success; inspect retained command logs. No GUI acceptance claim.','logs':[str(p) for p in out.glob('*.log')] or [str(out/'start.json')]}
        (out/'failure.json').write_text(json.dumps(result))
        subprocess.run(['node','tools/validation-reuse.mjs','finish',str(out/'start.json'),str(out/'failure.json'),str(out/'receipt.json')],check=False)
atexit.register(preserve_failure)
def run(args, timeout=120):
    result = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
    with (out/'commands.log').open('a') as log: log.write(result.stdout + result.stderr)
    result.check_returncode()
    return result.stdout
def write(name, obj):
    (out / name).write_text(json.dumps(obj, indent=2) + '\n')
assert os.environ['GITHUB_RUN_ATTEMPT'] == '1'
with tempfile.TemporaryDirectory() as name, contextlib.ExitStack() as cleanup:
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
    daemon_archive=tmp/'daemon.tar.gz'
    run(['openssl','cms','-decrypt','-binary','-inform','DER','-in','daemon-encrypted/macos-evidence.cms','-inkey',str(key),'-out',str(daemon_archive)])
    target=os.environ['DAEMON_TARGET']
    daemon_entry=json.loads(pathlib.Path('validation-inputs/daemon-signing-plan.json').read_text())['targets'][target]
    daemon=tmp/'burst-daemon'
    with tarfile.open(daemon_archive) as tar:
        daemon.write_bytes(tar.extractfile(daemon_entry['root']+'/burst-daemon').read())
    assert hashlib.sha256(daemon.read_bytes()).hexdigest()==daemon_entry['sha256']
    daemon.chmod(0o755)
    for port in [7077,29075]:
        with socket.socket() as sock: sock.bind(('127.0.0.1',port))
    daemon_log=(out/'daemon.log').open('w')
    daemon_process=subprocess.Popen([str(daemon),'--network','dev','--no-representative','--data-dir',str(tmp/'ledger'),'--rpc=true','--rpc-port','7077','--port','29075','--websocket=false','--disable-upnp','node','run'],stdout=daemon_log,stderr=daemon_log,start_new_session=True)
    def stop_daemon():
        if daemon_process.poll() is None:
            os.killpg(daemon_process.pid,signal.SIGTERM)
            try: daemon_process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(daemon_process.pid,signal.SIGKILL);daemon_process.wait(timeout=5)
        daemon_log.close()
    cleanup.callback(stop_daemon)
    baseline=None
    for attempt in range(50):
        try:
            request=urllib.request.Request('http://127.0.0.1:7077',data=b'{"action":"node_info"}',headers={'Content-Type':'application/json'})
            with urllib.request.urlopen(request,timeout=2) as response:
                envelope=json.load(response);baseline=envelope.get('result',envelope)
            break
        except OSError:
            assert daemon_process.poll() is None, 'Dev daemon exited'
            time.sleep(0.1)
    assert baseline and baseline['network']=='dev'
    write('rpc-baseline.json',baseline)
    prior = tmp / 'macos-signing-evidence'
    plan = json.loads((prior/'plan.json').read_text())
    compiler = run(['xcrun','--find','swiftc']).strip()
    run(['xcrun','swiftc','-j','1','tools/macos-gui-environment.swift','-o',str(out/'probe')], timeout=180)
    write('environment.json', {'runner':os.environ['RUNNER_OS'], 'architecture':os.environ['RUNNER_ARCH'], 'sourceRun':os.environ['PRIOR_RUN'], 'scope':'Desktop session and Accessibility availability only; no GUI acceptance', 'compiler':compiler})
    for app, expected in plan['apps'].items():
        if app != 'node-gui': continue
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
                probe_output = run([str(out/'probe'),str(process.pid),app])
                state = json.loads(probe_output.split('GUI_PASS')[0])
                assert 'GUI_PASS' in probe_output
                state['guiAssertions'] = probe_output.split('GUI_PASS',1)[1].strip()
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
    stop_daemon()

write('result.json', {'outcome':'pass','exitCode':0,'assertions':8,'review':'Exact notarized dashboard archive verified, fresh installed profile, strict signature/Gatekeeper and native connection dialog assertions. Live disposable Dev RPC bridge rendered isolated-node status and Devnet disclosure. Earlier wallet partial pass retained; wallet not rerun. Live RPC and full wallet lifecycle remain outside scope.','logs':[str(out/'node-gui.json'),str(out/'environment.json'),str(out/'node-gui.launch.log'),str(out/'commands.log'),str(out/'rpc-baseline.json')]})
run(['node','tools/validation-reuse.mjs','finish',str(out/'start.json'),str(out/'result.json'),str(out/'receipt.json')])
