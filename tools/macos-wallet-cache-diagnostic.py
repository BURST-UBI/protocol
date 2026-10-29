"""Read preserved Intel app unit timestamps; never compile or change the cache."""
import datetime,json,pathlib
root=pathlib.Path(__file__).resolve().parent.parent
out=root/'macos-wallet-cache-diagnostic';out.mkdir(exist_ok=False)
cache=root/'.macos-target/x86_64-apple-darwin/release'
records=[]
for name in ['burst-wallet-core','burst-wallet-gui','burst-ledger','burst-governance','tauri','wry']:
 for p in (cache/'build'/name).rglob('*'):
  if p.is_file():
   stat=p.stat()
   if 'fingerprint' in p.parts or p.suffix in ['.rlib','.rmeta']:
    records.append({'path':str(p.relative_to(cache)),'bytes':stat.st_size,'modifiedUtc':datetime.datetime.fromtimestamp(stat.st_mtime,datetime.timezone.utc).isoformat()})
(out/'cache-files.json').write_text(json.dumps(records,indent=2)+'\n')
print('Read-only Intel wallet diagnostic:',len(records),'retained records; no build executed.')
