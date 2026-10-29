"""Validate the unchanged Intel DMG using approved ARM runner network egress."""
import json, os, pathlib, subprocess, tarfile, tempfile, hashlib, platform, shutil
root=pathlib.Path.cwd();out=root/'macos-daemon-gatekeeper-evidence';out.mkdir()
def write(name,value): (out/name).write_text(json.dumps(value,indent=2)+'\n')
assert os.environ['GITHUB_RUN_ATTEMPT']=='1'
assert platform.machine()=='arm64'
expected='9976823dd0984602299a62c7a4ff3ba45cbb129bcf5579eb6c7cb7473d28e1f1'
with tempfile.TemporaryDirectory() as name:
 tmp=pathlib.Path(name);key=tmp/'key.pem';key.write_text(os.environ.pop('EVIDENCE_DECRYPT_KEY'));key.chmod(0o600)
 subprocess.run(['openssl','cms','-decrypt','-binary','-inform','DER','-in','prior-encrypted/macos-evidence.cms','-inkey',str(key),'-out',str(tmp/'evidence.tar.gz')],check=True)
 with tarfile.open(tmp/'evidence.tar.gz') as tar:
  for member in tar.getmembers():
   path=pathlib.Path(member.name)
   assert not path.is_absolute() and '..' not in path.parts and path.parts[0]=='macos-signing-evidence'
   assert member.isfile() or member.isdir()
  tar.extractall(root,filter='data')
prior=root/'macos-signing-evidence'
image=prior/'Burst-Daemon.dmg'
assert hashlib.sha256(image.read_bytes()).hexdigest()==expected
assert json.loads((prior/'daemon.notary-info.json').read_text())['status']=='Accepted'
write('host.json',{'architecture':platform.machine(),'os':platform.platform(),'runner':os.environ['RUNNER_NAME'],'runnerOS':os.environ['RUNNER_OS'],'stapler':subprocess.check_output(['xcrun','--find','stapler'],text=True).strip()})
plan={'requirement':'Validate the existing notarization ticket on the exact Intel daemon DMG','scope':'Only unresolved ticket validator using ARM host egress; no Intel execution, signing, stapling, submission or repeated Gatekeeper test','command':['python3','tools/macos-daemon-gatekeeper.py'],'inputs':['tools/macos-daemon-gatekeeper.py','prior-encrypted/macos-evidence.cms',str(out/'host.json')],'context':{'toolchain':'Apple stapler; no compiler','features':'Unchanged notarized Intel DMG SHA256 '+expected,'network':'Approved ARM runner egress replaces known failing macos-15-intel CloudKit route','topology':'GitHub macos-15 ARM, previously approved; no Azure','limits':'One diagnostic validation attempt;120s command cap; no credentials beyond evidence decryption'},'changeImpact':[{'changedDependency':'Host architecture and network egress; actual host observation is an immutable input','affectedCheck':'Public Apple CloudKit ticket lookup by stapler validate','reusableReceiptArtifact':'Latest failed collection40877414418; separate Intel Gatekeeper40878120775 and native Intel runtime40864042513 pass','reuseOrRerun':'Validate exact unchanged Intel image on ARM; preserve previous failures and retry budget','reason':'Official runner repository issue14924 reports the matching macos-15-intel NSURLErrorDomain -1005 regression; discriminate host/network defect from malformed ticket. Signature-ticket checks do not execute the Intel payload.'}],'rerun':{'reason':'Real host/network change after diagnosed runner-specific issue14924, not another unchanged Intel retry','attemptBudget':1}}
write('plan.json',plan)
subprocess.run(['node','tools/validation-reuse.mjs','prepare',str(out/'plan.json'),str(prior/'receipt.json'),str(out/'start.json')],check=True)
try:
 result=subprocess.run(['xcrun','stapler','validate',str(image)],capture_output=True,text=True,timeout=120)
 (out/'ticket.log').write_text(result.stdout+result.stderr)
 print(result.stdout+result.stderr)
 result.check_returncode()
 assert hashlib.sha256(image.read_bytes()).hexdigest()==expected
 shutil.copy2(image,out/'Burst-Daemon-macOS-x86_64.dmg')
 terminal={'outcome':'pass','exitCode':0,'assertions':3,'review':'Exact Intel DMG ticket validation passed on ARM host, unchanged SHA256. Intel runtime/Gatekeeper scopes reused separately. No re-signing, restapling, submission or public release.','sha256':expected}
except Exception:
 terminal={'outcome':'fail','exitCode':1,'assertions':0,'review':'Changed-host ticket validation failed; inspect log. Do not repeat unchanged.'}
 raise
finally:
 terminal['logs']=[str(out/'ticket.log'),str(out/'host.json')]
 write('result.json',terminal)
 subprocess.run(['node','tools/validation-reuse.mjs','finish',str(out/'start.json'),str(out/'result.json'),str(out/'receipt.json')],check=True)
