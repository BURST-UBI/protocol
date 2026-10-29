"""Inspect the preserved Intel compiler stage and reproduce timeout cleanup; no builds."""
import datetime,json,os,pathlib,signal,subprocess,time
root=pathlib.Path(__file__).resolve().parent.parent
out=root/'macos-cache-diagnostic';out.mkdir(exist_ok=False)
cache=root/'.macos-target/x86_64-apple-darwin/release'
records=[]
for pat in ['build/burst-node/**/*','build/burst-daemon/**/*','burst-daemon']:
 for p in cache.glob(pat):
  if p.is_file():
   s=p.stat();records.append({'path':str(p.relative_to(cache)),'bytes':s.st_size,'modifiedUtc':datetime.datetime.fromtimestamp(s.st_mtime,datetime.timezone.utc).isoformat()})
(out/'cache-files.json').write_text(json.dumps(records,indent=2)+'\n')
result={'cargoLayout':'Rust1.101 nightly uses release/build/PACKAGE/HASH/out; old fingerprint layout search was inapplicable', 'platform':subprocess.check_output(['sw_vers'],text=True),'buildExecuted':False}
child=subprocess.Popen(['/usr/bin/nice','-n','10','/bin/sleep','30'],start_new_session=True)
started=time.monotonic()
try:
 child.wait(timeout=0.1)
except subprocess.TimeoutExpired:
 result['timeoutObserved']=True
 os.killpg(child.pid,signal.SIGTERM)
 child.wait(timeout=10)
 result['childExitCode']=child.returncode
 try:
  os.killpg(child.pid,signal.SIGKILL)
  result['postExitKill']='succeeded'
 except OSError as e:
  result['postExitKill']={'errno':e.errno,'type':type(e).__name__}
 lines=subprocess.check_output(['ps','-axo','uid=,pid=,pgid='],text=True).splitlines()
 result['remainingGroupMembers']=[line.strip() for line in lines if len(line.split())==3 and line.split()[2]==str(child.pid)]
finally:
 if child.poll() is None:
  child.kill();child.wait(timeout=10)
result['elapsedSeconds']=time.monotonic()-started
from macos_process import stop_group
child2=subprocess.Popen(['/bin/sleep','30'],start_new_session=True)
stop_group(child2)
result['correctedCleanupExitCode']=child2.returncode
(out/'cleanup.json').write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps({'cacheRecords':len(records),'cleanup':result}))
