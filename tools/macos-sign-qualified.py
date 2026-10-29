"""Sign frozen app bundles and retain Apple's submission IDs; never publish."""
import base64
import hashlib
import json
import os
from pathlib import Path
import plistlib
import secrets
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / 'macos-signing-evidence'
PLAN = json.loads((ROOT / 'validation-inputs/arm-signing-plan.json').read_text())
KEYCHAIN = Path(os.environ['RUNNER_TEMP']) / 'burst-signing.keychain-db'
PROFILE = 'burst-notarization'
SEARCH_STATE = Path(os.environ['RUNNER_TEMP']) / 'burst-original-keychain-search.json'
PRIVATE = [v for k, v in os.environ.items() if k in (
    'APPLE_P12_BASE64', 'APPLE_P12_PASSWORD', 'APPLE_ID', 'APPLE_APP_PASSWORD',
    'EVIDENCE_DECRYPT_KEY') and v]


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def write(path, value):
    with path.open('x') as stream:
        json.dump(value, stream, indent=2)
        stream.write('\n')


def run(label, args, timeout=120):
    # Never include credential-bearing argv in exceptions or terminal logs.
    try:
        result = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        raise RuntimeError(label + ' timed out; inspect saved submission before retry') from None
    output = result.stdout + result.stderr
    for value in PRIVATE:
        output = output.replace(value, '[redacted]')
    if result.returncode:
        if OUT.exists():
            (OUT / (label + '.error.log')).write_text(output)
        raise RuntimeError(label + ' failed; retained redacted diagnostic') from None
    return result.stdout


def extract(archive, destination, prefix):
    with tarfile.open(archive) as tar:
        for member in tar.getmembers():
            path = Path(member.name)
            assert not path.is_absolute() and '..' not in path.parts
            assert path.parts[0] == prefix and (member.isfile() or member.isdir())
        tar.extractall(destination, filter='data')


def prepare():
    assert os.environ.get('GITHUB_RUN_ATTEMPT') == '1', 'Do not resubmit unchanged jobs'
    OUT.mkdir(exist_ok=False)
    shutil.copy2(ROOT / 'validation-inputs/arm-signing-plan.json', OUT / 'plan.json')
    with tempfile.TemporaryDirectory(dir=os.environ['RUNNER_TEMP']) as name:
        tmp = Path(name)
        key = tmp / 'evidence.pem'
        key.write_text(os.environ['EVIDENCE_DECRYPT_KEY'])
        key.chmod(0o600)
        archive = tmp / 'packages.tar.gz'
        run('decrypt-package', ['openssl', 'cms', '-decrypt', '-binary', '-inform', 'DER',
            '-in', str(ROOT / 'prior-encrypted/macos-evidence.cms'), '-inkey', str(key),
            '-out', str(archive)])
        if PLAN.get('resumeFromRun'):
            assert digest(ROOT / 'prior-encrypted/macos-evidence.cms') == PLAN['resumeEncryptedSha256']
            extract(archive, tmp, 'macos-signing-evidence')
            prior = tmp / 'macos-signing-evidence'
            old_plan = json.loads((prior / 'plan.json').read_text())
            assert old_plan['apps'] == PLAN['apps'] and old_plan['certificateSha1'] == PLAN['certificateSha1']
            assert not list(prior.glob('*.submission*')), 'Review prior submission IDs instead of resubmitting'
            shutil.copytree(prior, ROOT / 'prior-signing-evidence')
            for app, expected in PLAN['apps'].items():
                bundle = prior / expected['bundleName']
                info = plistlib.loads((bundle / 'Contents/Info.plist').read_bytes())
                assert info['CFBundleIdentifier'] == expected['bundleIdentifier']
                executable = info['CFBundleExecutable']
                assert Path(executable).name == executable
                expected_hash = PLAN.get('resumeSignedApps', {}).get(app, expected['binarySha256'])
                assert digest(bundle / 'Contents/MacOS' / executable) == expected_hash
                shutil.copytree(bundle, OUT / expected['bundleName'])
            write(OUT / 'input-review.json', {'outcome': 'pass', 'sourceRun': PLAN['resumeFromRun'],
                'assertions': 'Exact encrypted prior evidence, candidate identities and retained signed/raw binary hashes; no prior submission'})
            return
        extract(archive, tmp, 'macos-package-evidence')
        for app, expected in PLAN['apps'].items():
            package = tmp / 'macos-package-evidence' / (app + '.app.tar.gz')
            assert digest(package) == expected['archiveSha256'], 'Unsigned package changed'
            extract(package, OUT, expected['bundleName'])
            bundle = OUT / expected['bundleName']
            info = plistlib.loads((bundle / 'Contents/Info.plist').read_bytes())
            assert info['CFBundleIdentifier'] == expected['bundleIdentifier']
            executable = info['CFBundleExecutable']
            assert Path(executable).name == executable
            binary = bundle / 'Contents/MacOS' / executable
            assert digest(binary) == expected['binarySha256'], 'Qualified binary changed'
            files = {str(p.relative_to(bundle)) for p in bundle.rglob('*') if p.is_file()}
            assert files == {'Contents/Info.plist', 'Contents/Resources/icon.icns',
                             'Contents/MacOS/' + executable}, 'Review unexpected embedded code'
    write(OUT / 'input-review.json', {'outcome': 'pass', 'sourceRun': PLAN['sourceRun'],
        'assertions': 'Both archive hashes, bundle identifiers, raw binary hashes and exact contents'})


def credentials():
    assert not KEYCHAIN.exists(), 'Refuse to replace an existing keychain'
    password = secrets.token_urlsafe(32)
    PRIVATE.append(password)
    run('create-keychain', ['security', 'create-keychain', '-p', password, str(KEYCHAIN)])
    run('unlock-keychain', ['security', 'unlock-keychain', '-p', password, str(KEYCHAIN)])
    run('keychain-timeout', ['security', 'set-keychain-settings', '-lut', '3600', str(KEYCHAIN)])
    original_search = shlex.split(run('read-keychain-search', ['security', 'list-keychains', '-d', 'user']))
    write(SEARCH_STATE, original_search)
    SEARCH_STATE.chmod(0o600)
    run('register-keychain', ['security', 'list-keychains', '-d', 'user', '-s', str(KEYCHAIN), *original_search])
    with tempfile.TemporaryDirectory(dir=os.environ['RUNNER_TEMP']) as name:
        p12 = Path(name) / 'identity.p12'
        p12.write_bytes(base64.b64decode(os.environ['APPLE_P12_BASE64'], validate=True))
        p12.chmod(0o600)
        run('import-key', ['security', 'import', str(p12), '-k', str(KEYCHAIN), '-P',
            os.environ['APPLE_P12_PASSWORD'], '-T', '/usr/bin/codesign'])
    run('key-access', ['security', 'set-key-partition-list', '-S', 'apple-tool:,apple:,codesign:',
        '-s', '-k', password, str(KEYCHAIN)])
    identities = run('find-identity', ['security', 'find-identity', '-v', '-p', 'codesigning', str(KEYCHAIN)])
    write(OUT / 'identity-review.json', {'matchingCertificate': PLAN['certificateSha1'] in identities,
        'certificateSha1': PLAN['certificateSha1'], 'scope': 'Native code-signing identity lookup'})
    assert PLAN['certificateSha1'] in identities, 'Expected new certificate unavailable'
    run('notary-credentials', ['xcrun', 'notarytool', 'store-credentials', PROFILE,
        '--apple-id', os.environ['APPLE_ID'], '--team-id', PLAN['teamId'],
        '--password', os.environ['APPLE_APP_PASSWORD'], '--keychain', str(KEYCHAIN)])


def cleanup():
    try:
        if SEARCH_STATE.exists():
            run('restore-keychain-search', ['security', 'list-keychains', '-d', 'user', '-s',
                *json.loads(SEARCH_STATE.read_text())])
    finally:
        if KEYCHAIN.exists():
            run('delete-keychain', ['security', 'delete-keychain', str(KEYCHAIN)])


def app_stage(app):
    expected = PLAN['apps'][app]
    bundle = OUT / expected['bundleName']
    assert (OUT / 'input-review.json').is_file()
    assert not (OUT / (app + '.submission.json')).exists(), 'Never duplicate a saved submission'
    started = time.monotonic()
    result = {'outcome': 'incomplete', 'scope': 'Signing/notarization only; no GUI acceptance'}
    try:
        if app not in PLAN.get('resumeSignedApps', {}):
            run(app + '-sign', ['codesign', '--force', '--sign', PLAN['certificateSha1'],
                '--keychain', str(KEYCHAIN), '--options', 'runtime', '--timestamp', str(bundle)])
        run(app + '-verify', ['codesign', '--verify', '--deep', '--strict', '--verbose=2', str(bundle)])
        # Export the embedded leaf certificate and bind it to the selected new identity.
        cert = OUT / (app + '-certificate')
        run(app + '-certificate', ['codesign', '--display', '--extract-certificates=' + str(cert), str(bundle)])
        assert hashlib.sha1(Path(str(cert) + '0').read_bytes()).hexdigest().upper() == PLAN['certificateSha1']
        archive = OUT / (app + '.signed.zip')
        run(app + '-zip', ['ditto', '-c', '-k', '--keepParent', str(bundle), str(archive)])
        write(OUT / (app + '.signed.json'), {'sha256': digest(archive), 'unsigned': expected,
            'certificateSha1': PLAN['certificateSha1']})
        # Save the submission ID before any polling. An interrupted upload needs history review.
        (OUT / (app + '.submission-started')).write_text(str(time.time()) + '\n')
        response = run(app + '-submit', ['xcrun', 'notarytool', 'submit', str(archive),
            '--keychain-profile', PROFILE, '--keychain', str(KEYCHAIN), '--output-format', 'json'], timeout=600)
        submission = json.loads(response)
        write(OUT / (app + '.submission.json'), submission)
        identifier = submission['id']
        info = json.loads(run(app + '-info', ['xcrun', 'notarytool', 'info', identifier,
            '--keychain-profile', PROFILE, '--keychain', str(KEYCHAIN), '--output-format', 'json']))
        write(OUT / (app + '.notary-info.json'), info)
        result.update(submissionId=identifier, notaryStatus=info['status'])
        if info['status'] == 'Accepted':
            run(app + '-staple', ['xcrun', 'stapler', 'staple', str(bundle)])
            run(app + '-validate-ticket', ['xcrun', 'stapler', 'validate', str(bundle)])
            run(app + '-gatekeeper', ['spctl', '--assess', '--type', 'execute', '--verbose=4', str(bundle)])
            run(app + '-final-zip', ['ditto', '-c', '-k', '--keepParent', str(bundle),
                str(OUT / (app + '.notarized.zip'))])
            result.update(outcome='pass', finalSha256=digest(OUT / (app + '.notarized.zip')))
        elif info['status'] == 'Invalid':
            run(app + '-notary-log', ['xcrun', 'notarytool', 'log', identifier,
                '--keychain-profile', PROFILE, '--keychain', str(KEYCHAIN), str(OUT / (app + '.notary-log.json'))])
            raise RuntimeError('Apple rejected submission; inspect retained log')
        else:
            result['next'] = 'Query saved submission ID later; do not rebuild, re-sign or resubmit'
    except Exception as error:
        result.update(outcome='fail', error=type(error).__name__)
        raise
    finally:
        result['elapsedSeconds'] = time.monotonic() - started
        write(OUT / (app + '.result.json'), result)


if __name__ == '__main__':
    try:
        mode = sys.argv[1]
        if mode == 'prepare':
            prepare()
        elif mode == 'cleanup':
            cleanup()
        elif mode == 'credentials':
            credentials()
        elif mode in PLAN['apps']:
            app_stage(mode)
        else:
            raise ValueError('Unknown action')
    except Exception as error:
        # Exception reprs may contain subprocess argv; report only the class.
        print('Signing stage failed:', type(error).__name__, '; inspect encrypted evidence')
        sys.exit(1)
