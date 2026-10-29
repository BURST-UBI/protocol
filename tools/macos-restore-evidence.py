"""Restore the actual failed receipt chain privately; never manufacture prior results."""
from pathlib import Path
import os, subprocess, tarfile, tempfile
root=Path('macos-candidate-evidence')
(root/'host.json').rename(root/'host-current.json')
with tempfile.TemporaryDirectory(dir=os.environ['RUNNER_TEMP']) as temp:
    temp=Path(temp)
    key=temp/'evidence.key'
    key.write_text(os.environ['EVIDENCE_DECRYPT_KEY'])
    key.chmod(0o600)
    archive=temp/'prior.tar.gz'
    subprocess.run(['openssl','cms','-decrypt','-binary','-inform','DER','-in','prior-encrypted/macos-evidence.cms','-inkey',str(key),'-out',str(archive)],check=True)
    names={'daemon.plan.json','daemon.start.json','daemon.result.json','daemon.receipt.json','daemon.log','host.json'}
    with tarfile.open(archive) as tar:
        for member in tar.getmembers():
            if Path(member.name).name not in names: continue
            assert member.isfile() and member.name=='macos-candidate-evidence/'+Path(member.name).name
            out=root/Path(member.name).name
            assert not out.exists()
            with out.open('xb') as target: target.write(tar.extractfile(member).read())
    assert all((root/name).is_file() for name in names)
print('Prior failed stage files restored; next step audits hashes before prepare.')
