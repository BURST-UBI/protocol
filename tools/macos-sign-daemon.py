"""Package and sign previously qualified daemon bytes; never rebuild or publish."""
import json, os, pathlib, runpy, shutil, sys, tarfile, tempfile
shared=runpy.run_path(str(pathlib.Path(__file__).with_name('macos-sign-qualified.py')))
ROOT,OUT,PLAN,KEYCHAIN,PROFILE=[shared[n] for n in ('ROOT','OUT','PLAN','KEYCHAIN','PROFILE')]
run,write,digest=[shared[n] for n in ('run','write','digest')]
manifest=json.loads((ROOT/'validation-inputs/daemon-signing-plan.json').read_text())
entry=manifest['targets'][os.environ['DAEMON_TARGET']]
mode=sys.argv[1]
if mode in ('prepare','prepare-collection'):
 assert os.environ['GITHUB_RUN_ATTEMPT']=='1'
 with tempfile.TemporaryDirectory(dir=os.environ['RUNNER_TEMP']) as name:
  tmp=pathlib.Path(name);key=tmp/'key.pem';key.write_text(os.environ['EVIDENCE_DECRYPT_KEY']);key.chmod(0o600)
  archive=tmp/'source.tar.gz'
  run('decrypt-daemon',['openssl','cms','-decrypt','-binary','-inform','DER','-in',str(ROOT/'prior-encrypted/macos-evidence.cms'),'-inkey',str(key),'-out',str(archive)])
  shared['extract'](archive,ROOT,'macos-signing-evidence')
 if mode=='prepare':
  assert not list(OUT.glob('*.submission*')), 'Inspect prior submission before retry'
 else:
  assert json.loads((OUT/'daemon.notary-info.json').read_text())['status']=='Accepted'
  assert (OUT/'daemon.submission.json').exists()
 assert json.loads((OUT/'input-review.json').read_text())['source']==entry
 binary=OUT/'payload/burst-daemon'
 run('retained-signature',['codesign','--verify','--strict',str(binary)])
 cert=OUT/('retained-certificate-'+os.environ['GITHUB_RUN_ID'])
 run('retained-cert',['codesign','--display','--extract-certificates='+str(cert),str(binary)])
 import hashlib
 assert hashlib.sha1(pathlib.Path(str(cert)+'0').read_bytes()).hexdigest().upper()==PLAN['certificateSha1']
 old_receipt=ROOT/'prior-daemon-receipt.json'
 shutil.copy2(OUT/'receipt.json',old_receipt)
 receipt_plan={'requirement':manifest['requirement'],'scope':'Exact signed daemon and notarized DMG, no public release','command':['workflow','prepare','credentials','sign-submit','collect'],'inputs':['tools/macos-sign-daemon.py','tools/macos-sign-qualified.py','validation-inputs/daemon-signing-plan.json','validation-inputs/arm-signing-plan.json','prior-encrypted/macos-evidence.cms','LICENSE'],'context':{'toolchain':'Native Apple codesign/notarytool/hdiutil; no compiler','features':'Previously qualified standalone release daemon','network':'Apple notarization only; no ledger','topology':os.environ['DAEMON_TARGET']+' GitHub approved host; no Azure','limits':manifest['limits']},'changeImpact':manifest['changeImpact']}
 receipt_plan['rerun']={'reason':'Intel stapler validate failed NSURLErrorDomain -1005 network connection lost, error68; accepted DMG and original submission reused. One collection retry only.','attemptBudget':1}
 plan_path=OUT/('plan-'+os.environ['GITHUB_RUN_ID']+'.json')
 write(plan_path,receipt_plan)
 run('receipt-prepare',['node','tools/validation-reuse.mjs','prepare',str(plan_path),str(old_receipt),str(OUT/('start-'+os.environ['GITHUB_RUN_ID']+'.json'))])
 previous=OUT/('previous-'+os.environ['GITHUB_RUN_ID']);previous.mkdir()
 for name in ['receipt.json','identity-review.json','daemon.notary-info.json','daemon.result.json','resume-result.json']:
  if (OUT/name).exists(): (OUT/name).rename(previous/name)
elif mode=='sign-submit':
 assert (OUT/'input-review.json').exists()
 assert not (OUT/'daemon.submission-started').exists()
 binary=OUT/'payload/burst-daemon';image=OUT/'Burst-Daemon.dmg'
 run('daemon-strict',['codesign','--verify','--strict','--verbose=2',str(binary)])
 version=run('daemon-help',[str(binary),'--help'])
 assert 'BURST protocol node daemon' in version and '--network' in version
 run('daemon-dmg',['hdiutil','create','-volname','BURST Daemon','-srcfolder',str(OUT/'payload'),'-format','UDZO',str(image)])
 run('dmg-sign',['codesign','--sign',PLAN['certificateSha1'],'--keychain',str(KEYCHAIN),'--timestamp',str(image)])
 run('dmg-verify',['codesign','--verify','--strict',str(image)])
 write(OUT/'daemon.signed.json',{'binarySha256':digest(binary),'dmgSha256':digest(image),'certificateSha1':PLAN['certificateSha1'],'help':version})
 (OUT/'daemon.submission-started').write_text(os.environ['GITHUB_RUN_ID'])
 response=run('daemon-submit',['xcrun','notarytool','submit',str(image),'--keychain-profile',PROFILE,'--keychain',str(KEYCHAIN),'--output-format','json'],timeout=600)
 write(OUT/'daemon.submission.json',json.loads(response))
elif mode=='collect':
 identifier=json.loads((OUT/'daemon.submission.json').read_text())['id']
 auth=['--keychain-profile',PROFILE,'--keychain',str(KEYCHAIN)]
 status=json.loads(run('daemon-info',['xcrun','notarytool','info',identifier,*auth,'--output-format','json']))
 if status['status']=='In Progress':
  import subprocess
  result=subprocess.run(['xcrun','notarytool','wait',identifier,*auth,'--timeout','600s','--output-format','json'],capture_output=True,text=True,timeout=620)
  (OUT/'daemon.wait.log').write_text(result.stdout+result.stderr)
  status=json.loads(run('daemon-info',['xcrun','notarytool','info',identifier,*auth,'--output-format','json']))
 write(OUT/'daemon.notary-info.json',status)
 if status['status']=='Accepted':
  image=OUT/'Burst-Daemon.dmg'
  run('daemon-staple',['xcrun','stapler','staple',str(image)])
  run('daemon-ticket',['xcrun','stapler','validate',str(image)])
  run('daemon-gatekeeper',['spctl','--assess','--type','open','--context','context:primary-signature','--verbose=4',str(image)])
  run('daemon-final-strict',['codesign','--verify','--strict',str(OUT/'payload/burst-daemon')])
  write(OUT/'daemon.result.json',{'outcome':'pass','archive':image.name,'sha256':digest(image),'scope':'Exact qualified CLI signed, --help startup, signed DMG Apple accepted/stapled/Gatekeeper. No public release.'})
 elif status['status']=='Invalid':
  run('daemon-rejection',['xcrun','notarytool','log',identifier,*auth,str(OUT/'daemon.notary-log.json')])
  raise RuntimeError('Apple rejection retained; do not resubmit')
 else:
  write(OUT/'daemon.result.json',{'outcome':'incomplete','notaryStatus':status['status'],'next':'Collect original submission later; never rebuild, resign or resubmit'})
elif mode=='finish':
 if (OUT/('start-'+os.environ['GITHUB_RUN_ID']+'.json')).exists():
  result=json.loads((OUT/'daemon.result.json').read_text()) if (OUT/'daemon.result.json').exists() else {'outcome':'fail'}
  passed=result['outcome']=='pass'
  logs=[str(p) for p in OUT.glob('*.json') if p.name not in ('receipt-start.json','receipt-plan.json')]+[str(p) for p in OUT.glob('*.log')]
  terminal={'outcome':result['outcome'],'exitCode':0 if passed else 1,'assertions':6 if passed else 0,'review':result.get('scope',result.get('next','Signing or collection incomplete; inspect retained encrypted evidence; no release pass')),'logs':logs}
  write(OUT/'resume-result.json',terminal)
  run('receipt-finish',['node','tools/validation-reuse.mjs','finish',str(OUT/('start-'+os.environ['GITHUB_RUN_ID']+'.json')),str(OUT/'resume-result.json'),str(OUT/'receipt.json')])
else: raise ValueError('Unknown stage')
