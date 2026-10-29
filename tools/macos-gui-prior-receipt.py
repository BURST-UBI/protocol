"""Finalize retained original failed preflight before chained corrected attempt."""
import json, os, pathlib, subprocess, tarfile, tempfile
root=pathlib.Path.cwd()
prior=root/'prior-gui'; prior.mkdir()
with tempfile.TemporaryDirectory() as name:
 tmp=pathlib.Path(name);key=tmp/'key.pem';key.write_text(os.environ['EVIDENCE_DECRYPT_KEY']);key.chmod(0o600)
 archive=tmp/'prior.tar.gz'
 subprocess.run(['openssl','cms','-decrypt','-binary','-inform','DER','-in','prior-gui-encrypted/macos-evidence.cms','-inkey',str(key),'-out',str(archive)],check=True)
 with tarfile.open(archive) as tar:
  start=json.load(tar.extractfile('macos-gui-environment-evidence/start.json'))
 (prior/'start.json').write_text(json.dumps(start))
 assert start['cwd']==str(root)
 driver=root/'tools/macos-gui-environment.py';current=driver.read_bytes()
 try:
  driver.write_bytes((root/'validation-inputs/gui-prior-driver.py').read_bytes())
  result={'outcome':'fail','exitCode':1,'assertions':0,'review':'Original run40873182316 failed before app launch in direct Swift compiler. Exact original input inventory restored solely to finalize retained failed start. Diagnostic40873559826 reproduces missing standard library using direct invocation; xcrun succeeds.','logs':['validation-inputs/gui-prior-actions.log','prior-gui/start.json']}
  (prior/'result.json').write_text(json.dumps(result))
  subprocess.run(['node','tools/validation-reuse.mjs','finish',str(prior/'start.json'),str(prior/'result.json'),str(prior/'receipt.json')],check=True)
 finally: driver.write_bytes(current)
