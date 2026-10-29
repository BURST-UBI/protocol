"""Package qualified Mac binaries with Tauri's bundler; no rebuilding or signing."""
import hashlib,json,os,pathlib,plistlib,shutil,subprocess,sys,tarfile,time
from macos_process import stop_group
root=pathlib.Path(__file__).resolve().parent.parent
source=root/'candidate-source'
evidence=root/'macos-package-evidence'
prior=root/'macos-desktop-native-retry-evidence'
inputs=root/'validation-inputs'
node=str(pathlib.Path(shutil.which('node')).resolve())
checker=source/'tools/validation-reuse.mjs'
target=os.environ['MACOS_TARGET']
cache=root/'.macos-target'
os.environ['CARGO_TARGET_DIR']=str(cache)
os.environ['RUSTUP_TOOLCHAIN']='nightly-2026-10-11'
qualified=json.loads((inputs/'arm-native-artifacts.json').read_text())
manifest=json.loads((inputs/'desktop-source-manifest.json').read_text())
def write(p,v):
 with p.open('x') as f:json.dump(v,f,indent=2);f.write('\n')
def digest(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def receipt(*args):subprocess.run([node,str(checker),*map(str,args)],cwd=source,check=True,timeout=120)
mode=sys.argv[1]
if mode=='prepare':
 assert target=='aarch64-apple-darwin'
 subprocess.run([sys.executable,str(root/'tools/macos-desktop-native-retry.py'),'prepare'],check=True)
 (root/'macos-desktop-native-retry-evidence').rename(root/'macos-package-input-preflight')
 evidence.mkdir(exist_ok=False)
 shutil.copy2(root/'macos-package-input-preflight/host.json',evidence/'host.json')
 raise SystemExit(0)
if mode=='audit':
 stages=[{'name':app+'-native','plan':str(prior/(app+'-native.plan.json')),'receipt':str(prior/(app+'-native.receipt.json'))} for app in qualified['apps']]
 write(evidence/'stages.json',stages)
 receipt('audit',evidence/'stages.json')
 for app,artifact in qualified['apps'].items():
  result=json.loads((prior/(app+'-native.result.json')).read_text())
  assert result['outcome']=='pass' and result['exitCode']==0
  for binary in [prior/artifact['binary'],cache/target/'release'/artifact['binary']]:assert digest(binary)==artifact['sha256']
 write(evidence/'audit.json',{'outcome':'pass','run':qualified['run'],'scope':'Unchanged qualified binary/source identities only'})
 raise SystemExit(0)
app=mode;assert app in qualified['apps']
assert json.loads((evidence/'audit.json').read_text())['outcome']=='pass'
assert not (root/'Cargo.toml').exists()
appdir=root/'desktop-source'/app
artifact=qualified['apps'][app]
binary=cache/target/'release'/artifact['binary']
assert digest(binary)==artifact['sha256']
cargo=subprocess.check_output(['rustup','which','cargo'],text=True).strip()
command=[node,str(appdir/'node_modules/@tauri-apps/cli/tauri.js'),'bundle','--ci','--target',target,'--bundles','app','--no-sign']
p=evidence/app
plan={'requirement':app+' native ARM application packaging','scope':'Unsigned .app from exact qualified release binary; no compilation, Developer ID, notarization or GUI acceptance','command':command,'workingDirectory':str(appdir),
 'inputs':[str(source),str(binary),str(appdir/'dist'),node,cargo,str(pathlib.Path(__file__).resolve()),str(root/'tools/macos_process.py'),str(evidence/'audit.json'),str(evidence/'host.json'),str(inputs/'arm-native-artifacts.json'),str(inputs/'package-plan.json'),str(prior/(app+'-native.receipt.json'))]+[str(root/'desktop-source'/n) for n in manifest['files'] if n.startswith(app+'/')]+sorted({str(f.resolve()) for f in (appdir/'node_modules/@tauri-apps').rglob('*') if f.is_file()}),
 'context':{'toolchain':'Original frozen Tauri CLI, Node22, pinned Cargo metadata only; native Mac SDK captured','features':'Same compiled application; bundle --no-sign only','network':'No public deployment; no Apple credentials','topology':'Approved ARM GitHub host; Intel work remains on separate host','limits':'Command1200s+10s cleanup; no Cargo build, no Azure'},'changeImpact':json.loads((inputs/'package-plan.json').read_text())['changeImpact']}
write(p.with_suffix('.plan.json'),plan);receipt('begin',p.with_suffix('.plan.json'),p.with_suffix('.start.json'))
started=time.monotonic();code=1;review=plan['scope'];logs=[str(p.with_suffix('.log'))]
try:
 with p.with_suffix('.log').open('x') as log:
  child=subprocess.Popen(['/usr/bin/nice','-n','10',*command],cwd=appdir,stdout=log,stderr=subprocess.STDOUT,start_new_session=True)
  try:code=child.wait(timeout=1200)
  except subprocess.TimeoutExpired:code=124;stop_group(child)
 if code==0:
  bundle=cache/target/'release/bundle/macos'/(artifact['productName']+'.app')
  plist=plistlib.loads((bundle/'Contents/Info.plist').read_bytes())
  executable=plist['CFBundleExecutable'];assert '/' not in executable and executable not in ('.','..')
  assert digest(bundle/'Contents/MacOS'/executable)==artifact['sha256']
  assert digest(binary)==artifact['sha256'],'Bundling changed the qualified binary'
  config=json.loads((appdir/'src-tauri/tauri.conf.json').read_text())
  assert plist['CFBundleIdentifier']==config['identifier']
  archive=evidence/(app+'.app.tar.gz')
  with tarfile.open(archive,'w:gz') as t:t.add(bundle,arcname=bundle.name)
  write(p.with_suffix('.artifact.json'),{'archiveSha256':digest(archive),'binarySha256':artifact['sha256'],'bundleIdentifier':plist['CFBundleIdentifier'],'bundleName':bundle.name,'scope':review})
  logs += [str(archive),str(p.with_suffix('.artifact.json'))]
except Exception as e:
 code=code or 1;review+='; '+type(e).__name__+': '+str(e)
finally:
 result={'outcome':'pass' if code==0 else 'fail','exitCode':code,'assertions':3 if code==0 else 0,'elapsedSeconds':time.monotonic()-started,'review':review,'logs':logs}
 write(p.with_suffix('.result.json'),result);receipt('finish',p.with_suffix('.start.json'),p.with_suffix('.result.json'),p.with_suffix('.receipt.json'));receipt('check',p.with_suffix('.plan.json'),p.with_suffix('.receipt.json'))
print(json.dumps(result));raise SystemExit(code)
