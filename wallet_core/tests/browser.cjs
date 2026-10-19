// Real WASM in Chromium, with a local RPC contract server (not a BURST ledger).
const {test} = require('node:test');
const assert = require('node:assert/strict');
const http = require('node:http');
const fs = require('node:fs/promises');
const path = require('node:path');
const {createPublicKey, verify} = require('node:crypto');
const {chromium} = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const assets = path.resolve(__dirname,'../../target/browser-check');

test('browser wallet signs locally, confirms through Fetch, and bounds untrusted responses', {timeout:60_000}, async () => {
  let identity, block, redirectHits=0, aborted=0, mode='normal', cookies=[];
  let graphEdges=['member'],graphValid=true,graphRedirect=false;
  const requests=[];
  const server=http.createServer(async (req,res) => {
    cookies.push(req.headers.cookie);
    if(req.url==='/') {res.setHeader('Content-Type','text/html');res.end('<!doctype html><title>BURST WASM test</title>');return;}
    if(['/browser_check.js','/browser_check_bg.wasm'].includes(req.url)) {
      res.setHeader('Content-Type',req.url.endsWith('.wasm')?'application/wasm':'text/javascript');
      res.end(await fs.readFile(path.join(assets,req.url.slice(1))));return;
    }
    if(req.url==='/redirect') {res.writeHead(302,{Location:'/should-not-receive-wallet'});res.end();return;}
    if(req.url==='/should-not-receive-wallet') {redirectHits++;res.end('{}');return;}
    if(req.url==='/large') {res.writeHead(200,{'Content-Length':'100000'});res.end('x'.repeat(100000));return;}
    if(req.url==='/chunked') {res.write('a'.repeat(8));res.end('b'.repeat(8));return;}
    if(req.url==='/slow-body') {res.writeHead(200);res.write('a');res.on('close',()=>{if(!res.writableEnded)aborted++;});return;}
    if(req.url==='/groups/verify/browser-wallet') {res.end(JSON.stringify({valid:true,score:1}));return;}
    if(req.url.startsWith('/bad-groups/')) {res.writeHead(302,{Location:'/should-not-receive-wallet'});res.end();return;}
    if(req.url==='/graph-root/verify/browser-wallet') {res.end(JSON.stringify({valid:false,score:0}));return;}
    if(req.url==='/graph-root/trust') {
      if(graphRedirect) {res.writeHead(302,{Location:'/should-not-receive-wallet'});res.end();return;}
      res.end(JSON.stringify({version:1,groups:graphEdges}));return;
    }
    if(req.url==='/graph-member/verify/browser-wallet') {res.end(JSON.stringify({valid:graphValid,score:1}));return;}
    if(req.url!=='/rpc') {res.writeHead(404);res.end();return;}
    let body='';for await(const chunk of req) body+=chunk;
    const request=JSON.parse(body);requests.push(request);
    res.setHeader('Content-Type','application/json');
    if(mode==='slow-confirmation') {res.on('close',()=>{if(!res.writableEnded)aborted++;});return;}
    if(mode==='malformed') {res.end('not-json');return;}
    if(mode==='node-error') {res.end(JSON.stringify({error:'deliberate rejection'}));return;}
    if(mode==='http-error') {res.writeHead(503);res.end('{}');return;}
    let result;
    switch(request.action) {
      case 'account_info':result={head:'01'.repeat(32),block_count:1,representative:identity.address,
        payment_epoch:'02'.repeat(32),trst_balance:'9',release_sources:[],has_more_releases:false};break;
      case 'params_hash':result={block_version:6,payment_epoch:'02'.repeat(32),params_hash:'03'.repeat(32),min_work_difficulty:'0'};break;
      case 'work_generate':result={work:'0',hash:request.hash};break;
      case 'block_info':result={brn_balance:'0',confirmed:true};break;
      case 'process':block=JSON.parse(request.block);result={accepted:true,hash:Buffer.from(block.hash).toString('hex')};break;
      case 'ping':result={ok:true};break;
      default:res.writeHead(400);result={unexpected:request.action};
    }
    res.end(JSON.stringify({result}));
  });
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
  const base=`http://127.0.0.1:${server.address().port}`;
  const browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_PATH});
  const page=await browser.newPage();
  const failure=async (fn,args,pattern)=>{
    const result=await page.evaluate(async ({fn,args})=>{try {await window.api[fn](...args);return null;}catch(error){return String(error);}}, {fn,args});
    assert.match(result,pattern);
  };
  try {
    await page.goto(base);
    await page.evaluate(async ()=>{window.api=await import('/browser_check.js');await window.api.default();});
    const check=JSON.parse(await page.evaluate(()=>window.api.crypto_check()));
    assert.equal(check.distinct,true);assert.equal(check.signature_valid,true);assert.equal(check.work_valid,true);
    assert(Math.abs(check.now-Date.now()/1000)<5);
    identity=JSON.parse(await page.evaluate(()=>window.api.test_identity()));
    await page.context().addCookies([{url:base,name:'unrelated-session',value:'must-not-leak'}]);
    cookies=[];
    assert.deepEqual(JSON.parse(await page.evaluate(base=>window.api.rpc(base+'/rpc','ping'),base)),{ok:true});
    const hash=await page.evaluate(base=>window.api.pay(base+'/rpc'),base);
    assert.equal(block.block_type,'Burn');assert.equal(block.brn_balance,7);assert.equal(block.trst_balance,9);
    assert.equal(block.account,identity.address);assert.equal(hash,Buffer.from(block.hash).toString('hex'));
    const publicKey=createPublicKey({key:Buffer.concat([Buffer.from('302a300506032b6570032100','hex'),Buffer.from(identity.public_key,'hex')]),format:'der',type:'spki'});
    assert(verify(null,Buffer.from(block.hash),publicKey,Buffer.from(block.signature)));
    assert(requests.some(request=>request.action==='block_info'&&request.hash===hash));
    assert.equal(cookies.some(Boolean),false,'Fetch must not send ambient cookies');
    assert.equal(await page.evaluate(base=>window.api.group(base+'/groups'),base),true);
    const graph=async depth=>JSON.parse(await page.evaluate(({base,depth})=>window.api.group_graph(base,depth),{base,depth}));
    assert.equal(await graph(0),null);
    assert.deepEqual(await graph(1),['root','member']);
    graphEdges=[];assert.equal(await graph(1),null);
    graphEdges=['member'];graphValid=false;assert.equal(await graph(1),null);
    graphEdges=['root'];assert.equal(await graph(8),null);
    graphEdges=['unknown'];await failure('group_graph',[base,1],/not found/);
    graphEdges=['member'];graphValid=true;graphRedirect=true;
    await failure('group_graph',[base,1],/request.*failed/);
    graphRedirect=false;
    assert.equal(cookies.some(Boolean),false,'group traversal must not send ambient cookies');
    const workerResult=await page.evaluate(base=>new Promise((resolve,reject)=>{
      const source=`import init, {crypto_check,rpc,group_graph} from '${base}/browser_check.js';
        await init();postMessage({crypto:JSON.parse(crypto_check()),rpc:JSON.parse(await rpc('${base}/rpc','ping')),graph:JSON.parse(await group_graph('${base}',1))});`;
      const url=URL.createObjectURL(new Blob([source],{type:'text/javascript'}));
      const worker=new Worker(url,{type:'module'});
      const cleanup=()=>{worker.terminate();URL.revokeObjectURL(url);};
      worker.onmessage=event=>{cleanup();resolve(event.data);};
      worker.onerror=event=>{cleanup();reject(new Error(event.message));};
    }),base);
    assert.equal(workerResult.crypto.signature_valid,true);assert.equal(workerResult.crypto.work_valid,true);
    assert.deepEqual(workerResult.rpc,{ok:true});
    assert.deepEqual(workerResult.graph,['root','member']);
    await failure('bounded',[base+'/redirect',1000,64],/request failed/);
    await failure('group',[base+'/bad-groups'],/request.*failed/);
    assert.equal(redirectHits,0);
    await failure('bounded',[base+'/large',1000,8],/exceeds 8 bytes/);
    await failure('bounded',[base+'/chunked',1000,8],/exceeds 8 bytes/);
    await failure('bounded',[base+'/slow-body',50,64],/timed out/);
    for(const [next,pattern] of [['malformed',/invalid JSON/],['node-error',/deliberate rejection/],['http-error',/HTTP 503/]]) {
      mode=next;await failure('rpc',[base+'/rpc','ping'],pattern);
    }
    mode='slow-confirmation';
    const start=Date.now();await failure('wait_for_confirmation',[base+'/rpc'],/confirmation pending/);
    assert(Date.now()-start<1500,'confirmation deadline must cancel its in-flight RPC');
    await new Promise(resolve=>setTimeout(resolve,100));
    assert(aborted>=1,'body timeout must abort the Fetch connection');
  } finally {
    await browser.close();server.closeAllConnections();await new Promise(resolve=>server.close(resolve));
  }
});
