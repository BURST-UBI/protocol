"""Restore original failed frontend receipts intact, at their recorded paths."""
import os,pathlib,subprocess,tarfile,tempfile
root=pathlib.Path(__file__).resolve().parent.parent
assert not (root/'macos-candidate-evidence').exists()
with tempfile.TemporaryDirectory() as tmp:
    tmp=pathlib.Path(tmp);key=tmp/'key.pem';key.write_text(os.environ['EVIDENCE_DECRYPT_KEY']);key.chmod(0o600)
    archive=tmp/'prior.tar.gz'
    subprocess.run(['openssl','cms','-decrypt','-binary','-inform','DER','-in',str(root/'prior-encrypted/macos-evidence.cms'),'-inkey',str(key),'-out',str(archive)],check=True)
    with tarfile.open(archive) as t:
        for m in t.getmembers():
            p=pathlib.PurePosixPath(m.name)
            assert p.parts[0]=='macos-candidate-evidence' and '..' not in p.parts and not p.is_absolute()
            assert m.isfile() or m.isdir()
        t.extractall(root,filter='data')
for name in ['daemon','daemon-retry']:
    for ext in ['plan.json','result.json','receipt.json','log']:
        assert (root/'macos-candidate-evidence'/f'{name}.{ext}').is_file()
print('Restored original dependency and failed frontend evidence; no records altered.')
