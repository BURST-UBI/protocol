"""First independent Gatekeeper assessment after two ticket-validator failures."""
import json, os, pathlib, subprocess, tarfile, tempfile, hashlib
root=pathlib.Path.cwd();out=root/'macos-daemon-gatekeeper-evidence';out.mkdir()
def write(name,value): (out/name).write_text(json.dumps(value,indent=2)+'\n')
plan={'requirement':'Independent Gatekeeper acceptance of retained accepted/stapled Intel daemon image','scope':'First Gatekeeper and disk-image integrity checks; preserve unresolved stapler validate failure; no notarization retry','command':['python3','tools/macos-daemon-gatekeeper.py'],'inputs':['tools/macos-daemon-gatekeeper.py','prior-encrypted/macos-evidence.cms'],'context':{'toolchain':'Apple codesign/spctl/hdiutil','features':'Unchanged signed Intel daemon DMG','network':'No notary credentials; ordinary Gatekeeper evaluation','topology':'Approved GitHub macos-15-intel; no Azure','limits':'No compiler; each command120s; stage5minutes'},'changeImpact':[{'changedDependency':'First independent stage after ticket-validator network failure','affectedCheck':'Native Gatekeeper trust and image integrity','reusableReceiptArtifact':'Accepted/stapled image40877414418; original failed validator logs retained','reuseOrRerun':'First assessment, not a repeat ticket validation','reason':'Previous pipeline stopped at ticket validation before Gatekeeper; independent result cannot close the unresolved validator gate'}]}
write('plan.json',plan)
subprocess.run(['node','tools/validation-reuse.mjs','begin',str(out/'plan.json'),str(out/'start.json')],check=True)
try:
 with tempfile.TemporaryDirectory() as name:
  tmp=pathlib.Path(name);key=tmp/'key.pem';key.write_text(os.environ.pop('EVIDENCE_DECRYPT_KEY'));key.chmod(0o600)
  subprocess.run(['openssl','cms','-decrypt','-binary','-inform','DER','-in','prior-encrypted/macos-evidence.cms','-inkey',str(key),'-out',str(tmp/'evidence.tar.gz')],check=True)
  with tarfile.open(tmp/'evidence.tar.gz') as tar:
   status=json.load(tar.extractfile('macos-signing-evidence/daemon.notary-info.json'));assert status['status']=='Accepted'
   failure=tar.extractfile('macos-signing-evidence/daemon-ticket.error.log').read().decode();(out/'prior-validator-error.log').write_text(failure)
   print(failure)
   image=tmp/'Burst-Daemon.dmg';image.write_bytes(tar.extractfile('macos-signing-evidence/Burst-Daemon.dmg').read())
  write('input.json',{'priorRun':40877414418,'notaryStatus':'Accepted','sha256':hashlib.sha256(image.read_bytes()).hexdigest(),'remaining':'stapler validate failure remains unqualified'})
  for label,args in [('signature',['codesign','--verify','--strict',str(image)]),('image',['hdiutil','verify',str(image)]),('gatekeeper',['spctl','--assess','--type','open','--context','context:primary-signature','--verbose=4',str(image)])]:
   result=subprocess.run(args,capture_output=True,text=True,timeout=120)
   (out/(label+'.log')).write_text(result.stdout+result.stderr)
   print(label,result.returncode)
   result.check_returncode()
 result={'outcome':'pass','exitCode':0,'assertions':4,'review':'Apple Accepted, signed image integrity and first native Gatekeeper assessment passed. Online stapler validate still open; no public release.'}
except Exception:
 result={'outcome':'fail','exitCode':1,'assertions':0,'review':'Independent assessment failed; inspect logs. No repeated ticket validation or release pass.'}
 raise
finally:
 result['logs']=[str(p) for p in out.glob('*.log')]+([str(out/'input.json')] if (out/'input.json').exists() else [])
 write('result.json',result)
 subprocess.run(['node','tools/validation-reuse.mjs','finish',str(out/'start.json'),str(out/'result.json'),str(out/'receipt.json')],check=True)
