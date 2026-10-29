"""Restore latest completed environment receipt for the next native UI stage."""
import json, os, pathlib, subprocess, tarfile, tempfile
root=pathlib.Path.cwd()
prior=root/'prior-gui'; prior.mkdir()
with tempfile.TemporaryDirectory() as name:
 tmp=pathlib.Path(name);key=tmp/'key.pem';key.write_text(os.environ['EVIDENCE_DECRYPT_KEY']);key.chmod(0o600)
 archive=tmp/'prior.tar.gz'
 subprocess.run(['openssl','cms','-decrypt','-binary','-inform','DER','-in','prior-gui-encrypted/macos-evidence.cms','-inkey',str(key),'-out',str(archive)],check=True)
 with tarfile.open(archive) as tar:
  start=json.load(tar.extractfile('macos-gui-environment-evidence/start.json'))
  receipt=json.load(tar.extractfile('macos-gui-environment-evidence/receipt.json'))
  for path, mode, sha in receipt['evidence']:
   destination=pathlib.Path(path)
   assert destination.parent==root/'macos-gui-environment-evidence'
   member=tar.getmember('macos-gui-environment-evidence/'+destination.name)
   assert member.isfile()
   destination.write_bytes(tar.extractfile(member).read());destination.chmod(mode)
 (prior/'receipt.json').write_text(json.dumps(receipt))
 raise SystemExit(0)
