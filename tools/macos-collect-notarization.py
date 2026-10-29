"""Query existing Apple submissions and staple accepted apps; never re-sign or submit."""
import hashlib
import json
import os
from pathlib import Path
import runpy
import shutil
import subprocess
import sys
import tempfile

shared = runpy.run_path(str(Path(__file__).with_name('macos-sign-qualified.py')))
ROOT, OUT, PLAN, KEYCHAIN, PROFILE = [shared[n] for n in ('ROOT', 'OUT', 'PLAN', 'KEYCHAIN', 'PROFILE')]
run, write, digest, extract = [shared[n] for n in ('run', 'write', 'digest', 'extract')]
attempt = os.environ['GITHUB_RUN_ID']
assert attempt.isdigit()


def prepare():
    assert os.environ['GITHUB_RUN_ATTEMPT'] == '1'
    encrypted = ROOT / 'prior-encrypted/macos-evidence.cms'
    assert digest(encrypted) == os.environ['PRIOR_EVIDENCE_SHA256']
    with tempfile.TemporaryDirectory(dir=os.environ['RUNNER_TEMP']) as name:
        tmp = Path(name)
        key = tmp / 'evidence.pem'
        key.write_text(os.environ['EVIDENCE_DECRYPT_KEY'])
        key.chmod(0o600)
        archive = tmp / 'prior.tar.gz'
        run('decrypt-submissions', ['openssl', 'cms', '-decrypt', '-binary', '-inform', 'DER',
            '-in', str(encrypted), '-inkey', str(key), '-out', str(archive)])
        extract(archive, tmp, 'macos-signing-evidence')
        prior = tmp / 'macos-signing-evidence'
        previous_plan = json.loads((prior / 'plan.json').read_text())
        assert previous_plan['apps'] == PLAN['apps']
        assert previous_plan['certificateSha1'] == PLAN['certificateSha1']
        for app in PLAN['apps']:
            submission = json.loads((prior / (app + '.submission.json')).read_text())
            signed = json.loads((prior / (app + '.signed.json')).read_text())
            assert signed['certificateSha1'] == PLAN['certificateSha1']
            assert digest(prior / (app + '.signed.zip')) == signed['sha256']
            assert submission['id']
        shutil.copytree(prior, OUT)
    write(OUT / ('collection-plan-' + attempt + '.json'), {
        'sourceRun': os.environ['PRIOR_RUN'], 'evidenceSha256': os.environ['PRIOR_EVIDENCE_SHA256'],
        'scope': 'Poll original submission IDs; staple accepted exact signed packages; no signing, rebuild or resubmission',
        'limits': 'Each Apple wait600s and command cap620s; no Azure; no compiler',
    })


def collect(app):
    assert app in PLAN['apps']
    bundle = OUT / PLAN['apps'][app]['bundleName']
    submission = json.loads((OUT / (app + '.submission.json')).read_text())['id']
    prefix = app + '.collection-' + attempt
    result = {'outcome': 'incomplete', 'submissionId': submission,
              'scope': 'Signature, Apple notarization, stapling and Gatekeeper only; GUI acceptance separate'}
    auth = ['--keychain-profile', PROFILE, '--keychain', str(KEYCHAIN)]
    try:
        run(prefix + '-signature', ['codesign', '--verify', '--deep', '--strict', '--verbose=2', str(bundle)])
        cert = OUT / (prefix + '-certificate')
        run(prefix + '-certificate', ['codesign', '--display', '--extract-certificates=' + str(cert), str(bundle)])
        assert hashlib.sha1(Path(str(cert) + '0').read_bytes()).hexdigest().upper() == PLAN['certificateSha1']
        def info():
            return json.loads(run(prefix + '-info', ['xcrun', 'notarytool', 'info', submission,
                *auth, '--output-format', 'json']))
        status = info()
        if status['status'] == 'In Progress':
            waiting = subprocess.run(['xcrun', 'notarytool', 'wait', submission, *auth,
                '--timeout', '600s', '--output-format', 'json'], capture_output=True, text=True, timeout=620)
            (OUT / (prefix + '.wait.log')).write_text(waiting.stdout + waiting.stderr)
            status = info()
        write(OUT / (prefix + '.info.json'), status)
        result['notaryStatus'] = status['status']
        if status['status'] == 'Accepted':
            run(prefix + '-staple', ['xcrun', 'stapler', 'staple', str(bundle)])
            run(prefix + '-ticket', ['xcrun', 'stapler', 'validate', str(bundle)])
            run(prefix + '-strict', ['codesign', '--verify', '--deep', '--strict', '--verbose=2', str(bundle)])
            run(prefix + '-gatekeeper', ['spctl', '--assess', '--type', 'execute', '--verbose=4', str(bundle)])
            final = OUT / (app + '.notarized-' + attempt + '.zip')
            run(prefix + '-zip', ['ditto', '-c', '-k', '--keepParent', str(bundle), str(final)])
            result.update(outcome='pass', archive=final.name, sha256=digest(final))
        elif status['status'] == 'Invalid':
            run(prefix + '-rejection-log', ['xcrun', 'notarytool', 'log', submission, *auth,
                str(OUT / (prefix + '.notary-log.json'))])
            raise RuntimeError('Apple rejected this submission')
        else:
            result['next'] = 'Apple still processing; retain original submission ID and signed archive'
    except Exception as error:
        result.update(outcome='fail', error=type(error).__name__)
        raise
    finally:
        write(OUT / (prefix + '.result.json'), result)
        print(app, result['outcome'], result.get('notaryStatus', 'unknown'))


try:
    if sys.argv[1] == 'prepare':
        prepare()
    else:
        collect(sys.argv[1])
except Exception as error:
    print('Notarization collection failed:', type(error).__name__, '; inspect encrypted evidence')
    sys.exit(1)
