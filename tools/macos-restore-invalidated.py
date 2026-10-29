"""Restore original failed frontend receipts intact, at their recorded paths."""
import os,pathlib,subprocess,tarfile,tempfile
root=pathlib.Path(__file__).resolve().parent.parent
assert not (root/'macos-candidate-v3-evidence').exists()
with tempfile.TemporaryDirectory() as tmp:
    tmp=pathlib.Path(tmp);key=tmp/'key.pem';key.write_text(os.environ['EVIDENCE_DECRYPT_KEY']);key.chmod(0o600)
    archive=tmp/'prior.tar.gz'
    subprocess.run(['openssl','cms','-decrypt','-binary','-inform','DER','-in',str(root/'prior-encrypted/macos-evidence.cms'),'-inkey',str(key),'-out',str(archive)],check=True)
    with tarfile.open(archive) as t:
        for m in t.getmembers():
            p=pathlib.PurePosixPath(m.name)
            assert p.parts[0]=='macos-candidate-v3-evidence' and '..' not in p.parts and not p.is_absolute()
            assert m.isfile() or m.isdir()
        t.extractall(root,filter='data')
assert (root/'macos-candidate-v3-evidence/daemon-final.start.json').is_file()
assert (root/'macos-candidate-v3-evidence/daemon-final.result.json').is_file()
print('Restored latest invalidated attempt intact; no compiler run.')
