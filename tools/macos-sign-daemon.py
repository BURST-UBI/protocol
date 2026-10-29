"""Package and sign previously qualified daemon bytes; never rebuild or publish."""
import json, os, pathlib, runpy, shutil, sys, tarfile, tempfile
shared=runpy.run_path(str(pathlib.Path(__file__).with_name('macos-sign-qualified.py')))
ROOT,OUT,PLAN,KEYCHAIN,PROFILE=[shared[n] for n in ('ROOT','OUT','PLAN','KEYCHAIN','PROFILE')]
run,write,digest=[shared[n] for n in ('run','write','digest')]
manifest=json.loads((ROOT/'validation-inputs/daemon-signing-plan.json').read_text())
entry=manifest['targets'][os.environ['DAEMON_TARGET']]
mode=sys.argv[1]
if mode=='prepare':
 assert os.environ['GITHUB_RUN_ATTEMPT']=='1'
 OUT.mkdir()
 write(OUT/'plan.json',manifest)
 receipt_plan={'requirement':manifest['requirement'],'scope':'Exact signed daemon and notarized DMG, no public release','command':['workflow','prepare','credentials','sign-submit','collect'],'inputs':['tools/macos-sign-daemon.py','tools/macos-sign-qualified.py','validation-inputs/daemon-signing-plan.json','validation-inputs/arm-signing-plan.json','prior-encrypted/macos-evidence.cms','LICENSE'],'context':{'toolchain':'Native Apple codesign/notarytool/hdiutil; no compiler','features':'Previously qualified standalone release daemon','network':'Apple notarization only; no ledger','topology':os.environ['DAEMON_TARGET']+' GitHub approved host; no Azure','limits':manifest['limits']},'changeImpact':manifest['changeImpact']}
 write(OUT/'receipt-plan.json',receipt_plan)
 run('receipt-start',['node','tools/validation-reuse.mjs','begin',str(OUT/'receipt-plan.json'),str(OUT/'receipt-start.json')])
 with tempfile.TemporaryDirectory(dir=os.environ['RUNNER_TEMP']) as name:
  tmp=pathlib.Path(name);key=tmp/'key.pem';key.write_text(os.environ['EVIDENCE_DECRYPT_KEY']);key.chmod(0o600)
  archive=tmp/'source.tar.gz'
  run('decrypt-daemon',['openssl','cms','-decrypt','-binary','-inform','DER','-in',str(ROOT/'prior-encrypted/macos-evidence.cms'),'-inkey',str(key),'-out',str(archive)])
  payload=OUT/'payload';payload.mkdir()
  with tarfile.open(archive) as tar:
   member=tar.getmember(entry['root']+'/burst-daemon');assert member.isfile()
   binary=payload/'burst-daemon';binary.write_bytes(tar.extractfile(member).read());binary.chmod(0o755)
  assert digest(binary)==entry['sha256']
  shutil.copy2(ROOT/'LICENSE',payload/'LICENSE')
  (payload/'README.txt').write_text('BURST daemon for '+os.environ['DAEMON_TARGET']+'\n\nCopy burst-daemon to your preferred executable directory. Run ./burst-daemon --help for configuration. This disk image does not install or start a service.\n\nThis is a release qualification artifact, not an authorized public release.\n')
 write(OUT/'input-review.json',{'outcome':'pass','source':entry,'scope':'Exact previously qualified daemon bytes; native build/runtime evidence reused; no compiler'})
elif mode=='sign-submit':
 assert (OUT/'input-review.json').exists()
 assert not (OUT/'daemon.submission-started').exists()
 binary=OUT/'payload/burst-daemon';image=OUT/'Burst-Daemon.dmg'
 run('daemon-sign',['codesign','--force','--sign',PLAN['certificateSha1'],'--keychain',str(KEYCHAIN),'--options','runtime','--timestamp',str(binary)])
 run('daemon-strict',['codesign','--verify','--strict','--verbose=2',str(binary)])
 cert=OUT/'daemon-certificate'
 run('daemon-cert',['codesign','--display','--extract-certificates='+str(cert),str(binary)])
 import hashlib
 assert hashlib.sha1(pathlib.Path(str(cert)+'0').read_bytes()).hexdigest().upper()==PLAN['certificateSha1']
 version=run('daemon-version',[str(binary),'--version'])
 run('daemon-dmg',['hdiutil','create','-volname','BURST Daemon','-srcfolder',str(OUT/'payload'),'-format','UDZO',str(image)])
 run('dmg-sign',['codesign','--sign',PLAN['certificateSha1'],'--keychain',str(KEYCHAIN),'--timestamp',str(image)])
 run('dmg-verify',['codesign','--verify','--strict',str(image)])
 write(OUT/'daemon.signed.json',{'binarySha256':digest(binary),'dmgSha256':digest(image),'certificateSha1':PLAN['certificateSha1'],'version':version})
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
  write(OUT/'daemon.result.json',{'outcome':'pass','archive':image.name,'sha256':digest(image),'scope':'Exact qualified CLI signed, --version startup, signed DMG Apple accepted/stapled/Gatekeeper. No public release.'})
 elif status['status']=='Invalid':
  run('daemon-rejection',['xcrun','notarytool','log',identifier,*auth,str(OUT/'daemon.notary-log.json')])
  raise RuntimeError('Apple rejection retained; do not resubmit')
 else:
  write(OUT/'daemon.result.json',{'outcome':'incomplete','notaryStatus':status['status'],'next':'Collect original submission later; never rebuild, resign or resubmit'})
elif mode=='finish':
 if (OUT/'receipt-start.json').exists():
  result=json.loads((OUT/'daemon.result.json').read_text()) if (OUT/'daemon.result.json').exists() else {'outcome':'fail'}
  passed=result['outcome']=='pass'
  logs=[str(p) for p in OUT.glob('*.json') if p.name not in ('receipt-start.json','receipt-plan.json')]+[str(p) for p in OUT.glob('*.log')]
  terminal={'outcome':result['outcome'],'exitCode':0 if passed else 1,'assertions':6 if passed else 0,'review':result.get('scope',result.get('next','Signing or collection incomplete; inspect retained encrypted evidence; no release pass')),'logs':logs}
  write(OUT/'receipt-result.json',terminal)
  run('receipt-finish',['node','tools/validation-reuse.mjs','finish',str(OUT/'receipt-start.json'),str(OUT/'receipt-result.json'),str(OUT/'receipt.json')])
else: raise ValueError('Unknown stage')
