use super::*;
use burst_ledger::StateBlock;
use burst_transactions::Transaction;
use burst_types::TxHash;
use burst_wallet_core::transaction_builder::{build_burn_tx, build_receive_tx, build_state_block, sign_state_block};
use std::{collections::HashMap, sync::{Arc, Mutex}, time::{Duration, Instant}};
use tokio::sync::{mpsc, OwnedSemaphorePermit, Semaphore};

const DEADLINE: Duration = Duration::from_secs(25);
const MAX_IN_FLIGHT: usize = 1024;
type Report = Arc<Mutex<std::fs::File>>;

fn emit(report: &Report, event: Value) -> Result<()> {
    let mut file = report.lock().map_err(|_| "report lock poisoned")?;
    record(&mut file, event)
}

#[derive(Debug)]
struct Published {
    block: StateBlock,
    height: u64,
    started: Instant,
    preparation_ms: f64,
    accepted_ms: f64,
    observed: Vec<bool>,
    _permit: OwnedSemaphorePermit,
}

#[derive(Debug)]
struct Completed {
    head: BlockHash,
    blocks: u64,
}

fn duration(value: &str) -> Result<Duration> {
    let seconds: u64 = value.parse()?;
    if !(2..=30).contains(&seconds) { return Err("load duration must be 2..30 seconds".into()); }
    Ok(Duration::from_secs(seconds))
}

pub(in super::super) async fn execute(args: &[String]) -> Result<()> {
    if args.is_empty() { return Err("usage: --load SECONDS REPORT FIRST_PUBLIC_SEED ACCOUNTS URL URL [...]".into()); }
    let duration = duration(&args[0])?;
    let options = Options::parse(&args[1..])?;
    let mut file = OpenOptions::new().write(true).create_new(true).open(&options.report)?;
    record(&mut file, json!({"stage":"start","mode":"concurrent-burn-receive-load",
        "urls":options.urls,"first_public_seed":options.first_seed,"accounts":options.accounts,
        "requested_generation_seconds":duration.as_secs(),"max_in_flight":MAX_IN_FLIGHT,
        "deadline_seconds":DEADLINE.as_secs(),"clock":"real","work":"node generated, locally validated",
        "scope":"fixture RPC observations; external artifact/host evidence required; no automatic release approval"}))?;
    let ready = match inspect(&options, &mut file).await {
        Ok(ready) => ready,
        Err(error) => {
            record(&mut file,json!({"stage":"terminal","outcome":"fail","error":error.to_string(),"submitted":false}))?;
            return Err(error);
        }
    };
    let report = Arc::new(Mutex::new(file));
    let result = run(&options, ready, duration, report.clone()).await;
    emit(&report, json!({"stage":"terminal","outcome":if result.is_ok(){"pass"}else{"fail"},
        "error":result.as_ref().err().map(ToString::to_string),
        "scope":"functional assertions only; evaluate raw latency and throughput against separately recorded release requirements"}))?;
    result
}

async fn run(options: &Options, ready: Ready, duration: Duration, report: Report) -> Result<()> {
    let nodes: Vec<_> = options.urls.iter().map(NodeClient::new).collect::<std::result::Result<_,_>>()?;
    let ready = Arc::new(ready);
    let slots = Arc::new(Semaphore::new(MAX_IN_FLIGHT));
    let (sender, receiver) = mpsc::channel(256);
    let started = Instant::now();
    let until = started + duration;
    let producers = async {
        let mut tasks = tokio::task::JoinSet::new();
        for index in 0..options.accounts {
            let node = nodes[index % nodes.len()].clone();
            let wallet = Wallet::from_seed(&[options.first_seed + index as u8; 32]);
            let ready = ready.clone(); let sender = sender.clone();
            let slots = slots.clone(); let report = report.clone();
            tasks.spawn(async move {
                account_load(node, wallet, &ready, index, until, sender, slots, report).await.map(|done| (index, done))
            });
        }
        drop(sender);
        let mut counts: Vec<Option<Completed>> = (0..options.accounts).map(|_| None).collect();
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok((index, completed))) => counts[index] = Some(completed),
                failed => {
                    tasks.abort_all();
                    while tasks.join_next().await.is_some() {}
                    return Err(format!("load producer failed; signed attempts remain in report: {failed:?}").into());
                }
            }
        }
        let completed: Result<Vec<_>> = counts.into_iter().map(|count| count.ok_or_else(|| "missing producer result".into())).collect();
        emit(&report,json!({"stage":"producers-finished","elapsed_seconds":started.elapsed().as_secs_f64()}))?;
        completed
    };
    // Always join both sides. Dropping the observer closes the channel and releases
    // pending permits; producer failure aborts and drains every remaining worker.
    let (completed, observed) = tokio::join!(producers, observe(receiver, &nodes, &report));
    emit(&report,json!({"stage":"workers-finished",
        "producer_error":completed.as_ref().err().map(ToString::to_string),
        "observer_error":observed.as_ref().err().map(ToString::to_string)}))?;
    let (observations, peak) = observed?;
    let completed = completed?;
    let elapsed = started.elapsed().as_secs_f64();
    let total: u64 = completed.iter().map(|done| done.blocks).sum();
    if total != observations || completed.iter().any(|done| done.blocks == 0 || done.blocks % 2 != 0) {
        return Err("load requires complete payment pairs and one all-peer observation per signed block".into());
    }
    for (peer, node) in nodes.iter().enumerate() {
        let signing = node.rpc_call("params_hash",json!({})).await?;
        let actual = authority(&json!({"network":"dev"}), &signing, &ready.parameters)?;
        if actual != ready.authority { return Err("authority changed during load".into()); }
        for (index, done) in completed.iter().enumerate() {
            let baseline = &ready.baselines[index];
            let address = baseline["address"].as_str().ok_or("missing baseline address")?;
            let account = node.rpc_call("account_info",json!({"account":address})).await?;
            check_ending(baseline, &account, done)?;
            let wallet = Wallet::from_seed(&[options.first_seed + index as u8;32]);
            let state = node.signing_state(&wallet.address).await?;
            if state.head != done.head || state.payment_epoch != ready.authority.epoch
                || !state.signing_grant.is_zero() || !state.release_sources.is_empty() {
                return Err("ending signing authority or release state changed".into());
            }
            emit(&report,json!({"stage":"ending","peer":peer,"account":account}))?;
        }
    }
    emit(&report,json!({"stage":"load-summary","confirmed_blocks":total,"completed_pairs":total/2,
        "elapsed_with_drain_seconds":elapsed,"confirmed_blocks_per_second":total as f64/elapsed,
        "completed_pairs_per_second":(total/2) as f64/elapsed,"peak_in_flight":peak,
        "per_account_blocks":completed.iter().map(|done|done.blocks).collect::<Vec<_>>(),
        "note":"confirmation samples are RPC observation upper bounds; construction/work and observer overhead are recorded separately"}))
}

fn check_ending(baseline: &Value, account: &Value, done: &Completed) -> Result<()> {
    let rounds = u128::from(done.blocks / 2);
    let amount = rounds * (rounds + 1) / 2;
    let old_count = baseline["block_count"].as_u64().ok_or("missing baseline count")?;
    let expected_count = old_count.checked_add(done.blocks).ok_or("ending height overflow")?;
    let epoch: BlockHash = serde_json::from_value(baseline["payment_epoch"].clone())?;
    if done.blocks == 0 || done.blocks % 2 != 0
        || account["address"] != baseline["address"] || account["head"] != done.head.to_string()
        || account["block_count"].as_u64() != Some(expected_count)
        || account["confirmation_height"] != account["block_count"]
        || account["payment_epoch"] != epoch.to_string()
        || account["representative"] != baseline["representative"]
        || account["verified_at"] != baseline["verified_at"] || account["verification_state"] != "verified"
        || account["total_brn_staked"] != baseline["total_brn_staked"] {
        return Err("ending account identity, confirmation or unaffected economics changed".into());
    }
    for field in ["trst_balance", "total_brn_burned"] {
        let old: u128 = baseline[field].as_str().ok_or("missing baseline amount")?.parse()?;
        let actual: u128 = account[field].as_str().ok_or("missing ending amount")?.parse()?;
        if old.checked_add(amount) != Some(actual) { return Err(format!("ending {field} differs from exact signed payment total").into()); }
    }
    Ok(())
}

async fn account_load(node: NodeClient, wallet: Wallet, ready: &Ready, index: usize,
    until: Instant, sender: mpsc::Sender<Published>, slots: Arc<Semaphore>, report: Report) -> Result<Completed> {
    let baseline = &ready.baselines[index];
    let mut head: BlockHash = serde_json::from_value(baseline["head"].clone())?;
    let initial = baseline["block_count"].as_u64().ok_or("missing baseline height")?;
    let mut count = 0;
    for round in 1..=1024u128 {
        if Instant::now() >= until { return Ok(Completed { head, blocks: count }); }
        let burn = Transaction::Burn(build_burn_tx(&wallet.address,&wallet.address,round,Timestamp::now())?);
        let Some(hash) = publish(&node,&wallet,ready,burn,TxHash::ZERO,head,initial.checked_add(count).and_then(|height|height.checked_add(1)).ok_or("load height overflow")?,
            Some(until),&sender,&slots,&report).await? else { return Ok(Completed { head, blocks: count }); };
        head = hash; count += 1;
        let receive = Transaction::Receive(build_receive_tx(&wallet.address,hash.into_tx_hash(),round,Timestamp::now())?);
        head = publish(&node,&wallet,ready,receive,hash.into_tx_hash(),head,initial.checked_add(count).and_then(|height|height.checked_add(1)).ok_or("load height overflow")?,
            None,&sender,&slots,&report).await?.ok_or("receive unexpectedly skipped")?;
        count += 1;
    }
    if Instant::now() < until { return Err("bounded load fixture exhausted before requested duration".into()); }
    Ok(Completed { head, blocks: count })
}

async fn publish(node: &NodeClient, wallet: &Wallet, ready: &Ready, tx: Transaction,
    origin: TxHash, previous: BlockHash, height: u64, stop: Option<Instant>,
    sender: &mpsc::Sender<Published>, slots: &Arc<Semaphore>, report: &Report) -> Result<Option<BlockHash>> {
    let permit = slots.clone().acquire_owned().await?;
    if stop.is_some_and(|stop|Instant::now()>=stop) { return Ok(None); }
    if sender.is_closed() { return Err("confirmation observer stopped".into()); }
    let preparation_started = Instant::now();
    let block = tokio::time::timeout(DEADLINE, async {
        let state = node.signing_state(&wallet.address).await?;
        if state.head != previous || state.block_count.checked_add(1) != Some(height)
            || state.payment_epoch != ready.authority.epoch || !state.signing_grant.is_zero()
            || !state.release_sources.is_empty() {
            return Err("load signing state changed or requires release/device handling".into());
        }
        let signing = node.rpc_call("params_hash",json!({})).await?;
        if authority(&json!({"network":"dev"}),&signing,&ready.parameters)? != ready.authority {
            return Err("load authority changed before signing".into());
        }
        let work = node.prepare_work(&wallet.address).await?;
        let mut block = build_state_block(&state,&tx,origin,ready.authority.params)?;
        if work.root != block.work_root() || !burst_work::validate_work(&work.root,work.nonce,ready.authority.difficulty) {
            return Err("work does not match the signed account position and difficulty".into());
        }
        block = sign_state_block(block,&wallet.primary_keys.private);
        block.work = work.nonce;
        Ok::<_,Box<dyn Error+Send+Sync>>(block)
    }).await??;
    let preparation_ms = preparation_started.elapsed().as_secs_f64()*1000.0;
    // Flush the signed attempt to the report before submission. The report is
    // diagnostic output, not a crash-durable wallet journal (no fsync).
    let encoded = serde_json::to_string(&block)?;
    emit(report,json!({"stage":"signed","hash":block.hash.to_string(),"height":height,
        "block":encoded,"preparation_ms":preparation_ms}))?;
    if sender.is_closed() { return Err("confirmation observer stopped before submission".into()); }
    let started = Instant::now();
    let response = tokio::time::timeout_at((started+DEADLINE).into(),node.process(&encoded)).await??;
    if !response.accepted || response.hash != block.hash.to_string() {
        return Err(format!("submission rejected or hash mismatch for {}: {response:?}",block.hash).into());
    }
    let accepted_ms = started.elapsed().as_secs_f64()*1000.0;
    let hash = block.hash;
    let account = block.account.to_string();
    sender.send(Published { block,height,started,preparation_ms,accepted_ms,
        observed:Vec::new(),_permit:permit }).await?;
    tokio::time::timeout_at((started+DEADLINE).into(),async {
        loop {
            let state = node.account_info(&account).await?;
            if state.head == hash.to_string() {
                if state.block_count != height { return Err("admitted block has wrong height".into()); }
                return Ok::<_,Box<dyn Error+Send+Sync>>(());
            }
            if state.head != previous.to_string() { return Err("another writer changed load account".into()); }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await??;
    Ok(Some(hash))
}

fn check_confirmation(info: &Value, item: &Published) -> Result<bool> {
    if info["confirmed"] != true { return Ok(false); }
    let encoded = info["signed_block"].as_str().ok_or("confirmed RPC omitted signed block")?;
    let actual: StateBlock = serde_json::from_str(encoded)?;
    if actual.hash != item.block.hash || actual.compute_hash() != item.block.hash
        || serde_json::to_string(&actual)? != serde_json::to_string(&item.block)? {
        return Err("peer confirmed a different signed block".into());
    }
    Ok(true)
}

async fn observe(mut receiver: mpsc::Receiver<Published>, nodes: &[NodeClient], report: &Report) -> Result<(u64,usize)> {
    let mut pending: Vec<Published> = Vec::new();
    let mut closed = false; let mut count = 0; let mut peak = 0;
    let mut poll = tokio::time::interval(Duration::from_millis(25));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            item = receiver.recv(), if !closed => match item {
                Some(mut item) => { item.observed.resize(nodes.len(),false);pending.push(item);peak=peak.max(pending.len()); }
                None => closed=true,
            },
            _ = poll.tick(), if !pending.is_empty() => {
                let deadline = pending.iter().map(|item|item.started+DEADLINE).min().unwrap();
                if Instant::now() >= deadline { return Err("all-peer confirmation exceeded25s before poll".into()); }
                let poll_started = Instant::now();
                tokio::time::timeout_at(deadline.into(),async {
                    for (peer,node) in nodes.iter().enumerate() {
                        let mut heights=HashMap::new();
                        for item in pending.iter_mut().filter(|item|!item.observed[peer]) {
                            let address=item.block.account.to_string();
                            let height = if let Some(height)=heights.get(&address) { *height } else {
                                let account=node.account_info(&address).await?;
                                heights.insert(address,account.confirmation_height);account.confirmation_height
                            };
                            if height >= item.height {
                                let info=node.rpc_call("block_info",json!({"hash":item.block.hash.to_string(),"include_signed_block":true})).await?;
                                item.observed[peer]=check_confirmation(&info,item)?;
                            }
                        }
                    }
                    Ok::<_,Box<dyn Error+Send+Sync>>(())
                }).await??;
                emit(report,json!({"stage":"observer-poll","pending":pending.len(),"elapsed_ms":poll_started.elapsed().as_secs_f64()*1000.0}))?;
                let mut remaining=Vec::with_capacity(pending.len());
                for item in pending.drain(..) {
                    if item.started.elapsed() >= DEADLINE { return Err(format!("all-peer confirmation exceeded25s: {}",item.block.hash).into()); }
                    if item.observed.iter().all(|seen|*seen) {
                        emit(report,json!({"stage":"confirmed","hash":item.block.hash.to_string(),"height":item.height,
                            "operation":format!("{:?}",item.block.block_type),"preparation_ms":item.preparation_ms,
                            "accepted_ms":item.accepted_ms,"all_peer_observed_ms":item.started.elapsed().as_secs_f64()*1000.0}))?;
                        count+=1;
                    } else { remaining.push(item); }
                }
                pending=remaining;
            }
        }
        if closed && pending.is_empty() { return Ok((count,peak)); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn block() -> StateBlock {
        let wallet=Wallet::from_seed(&[1;32]);
        let state=AccountState { signing_grant:BlockHash::ZERO,payment_epoch:BlockHash::new([3;32]),
            release_sources:vec![],head:BlockHash::new([2;32]),block_count:1,
            representative:wallet.address.clone(),brn_spent:0,trst_balance:0 };
        let tx=Transaction::Burn(build_burn_tx(&wallet.address,&wallet.address,1,Timestamp::new(100)).unwrap());
        sign_state_block(build_state_block(&state,&tx,TxHash::ZERO,BlockHash::new([4;32])).unwrap(),&wallet.primary_keys.private)
    }
    async fn item(started: Instant) -> Published {
        Published { block:block(),height:2,started,preparation_ms:1.0,accepted_ms:0.5,observed:vec![],
            _permit:Arc::new(Semaphore::new(1)).acquire_owned().await.unwrap() }
    }
    #[test]
    fn load_duration_and_exact_ending_economics_are_checked() {
        for input in ["0","1","31","-1","abc"] { assert!(duration(input).is_err()); }
        assert_eq!(duration("20").unwrap(),Duration::from_secs(20));
        let epoch=BlockHash::new([3;32]);let head=BlockHash::new([5;32]);
        let before=json!({"address":"a","block_count":10,"trst_balance":"20","total_brn_burned":"30",
            "total_brn_staked":"4","representative":"r","verified_at":1,"payment_epoch":epoch,"signing_grant":BlockHash::ZERO});
        let after=json!({"address":"a","head":head.to_string(),"block_count":14,"confirmation_height":14,
            "trst_balance":"23","total_brn_burned":"33","total_brn_staked":"4","representative":"r","verified_at":1,
            "verification_state":"verified","payment_epoch":epoch.to_string()});
        let done=Completed { head,blocks:4 };
        check_ending(&before,&after,&done).unwrap();
        for (field,value) in [("confirmation_height",json!(13)),("trst_balance",json!("22")),("total_brn_burned",json!("34")),
            ("total_brn_staked",json!("3")),("payment_epoch",json!(head.to_string()))] {
            let mut invalid=after.clone();invalid[field]=value;assert!(check_ending(&before,&invalid,&done).is_err(),"{field}");
        }
        let mut overflow=before.clone();overflow["block_count"]=json!(u64::MAX);
        assert!(check_ending(&overflow,&after,&done).is_err());
    }
    #[tokio::test]
    async fn finality_requires_the_exact_signed_payload() {
        let item=item(Instant::now()).await;
        assert!(!check_confirmation(&json!({"confirmed":false}),&item).unwrap());
        assert!(check_confirmation(&json!({"confirmed":true}),&item).is_err());
        let info=json!({"confirmed":true,"signed_block":serde_json::to_string(&item.block).unwrap()});
        assert!(check_confirmation(&info,&item).unwrap());
        let mut fork=item.block.clone();fork.timestamp=Timestamp::new(101);fork.hash=fork.compute_hash();
        assert!(check_confirmation(&json!({"confirmed":true,"signed_block":serde_json::to_string(&fork).unwrap()}),&item).is_err());
        let mut altered=item.block.clone();altered.work=7;
        assert!(check_confirmation(&json!({"confirmed":true,"signed_block":serde_json::to_string(&altered).unwrap()}),&item).is_err());
    }
    #[tokio::test]
    async fn expired_observation_releases_permit_and_closes_producer_channel() {
        let directory=tempfile::tempdir().unwrap();let file=OpenOptions::new().write(true).create_new(true).open(directory.path().join("report")).unwrap();
        let report=Arc::new(Mutex::new(file));let (sender,receiver)=mpsc::channel(1);
        let slots=Arc::new(Semaphore::new(1));
        let mut expired=item(Instant::now()-DEADLINE).await;
        expired._permit=slots.clone().acquire_owned().await.unwrap();
        sender.send(expired).await.unwrap();
        let node=NodeClient::new("http://127.0.0.1:1").unwrap();
        let error=observe(receiver,&[node],&report).await.unwrap_err();
        assert!(error.to_string().contains("exceeded25s"));
        assert!(sender.is_closed());assert_eq!(slots.available_permits(),1);
    }
    async fn peer(confirmed: Arc<std::sync::atomic::AtomicBool>, block: StateBlock)
        -> (NodeClient,tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt,AsyncWriteExt};
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let node=NodeClient::new(format!("http://{}",listener.local_addr().unwrap())).unwrap();
        let server=tokio::spawn(async move {
            loop {
                let (mut stream,_)=listener.accept().await.unwrap();
                let mut request=Vec::new();
                let body=loop {
                    let mut bytes=[0;1024];let n=stream.read(&mut bytes).await.unwrap();assert!(n>0);
                    request.extend_from_slice(&bytes[..n]);assert!(request.len()<8192);
                    if let Some(end)=request.windows(4).position(|part|part==b"\r\n\r\n") {
                        let headers=std::str::from_utf8(&request[..end]).unwrap();
                        let length:usize=headers.lines().find_map(|line| {
                            let (key,value)=line.split_once(':')?;
                            key.eq_ignore_ascii_case("content-length").then(||value.trim().parse().unwrap())
                        }).unwrap();
                        if request.len()>=end+4+length { break serde_json::from_slice::<Value>(&request[end+4..end+4+length]).unwrap(); }
                    }
                };
                let ready=confirmed.load(std::sync::atomic::Ordering::SeqCst);
                let result=match body["action"].as_str().unwrap() {
                    "account_info"=>json!({"address":block.account,"head":block.hash.to_string(),"block_count":2,
                        "confirmation_height":if ready {2}else{1},"brn_balance":"1","trst_balance":"0",
                        "verification_state":"verified","representative":block.representative}),
                    "block_info"=>{
                        assert_eq!(body["hash"],block.hash.to_string());assert_eq!(body["include_signed_block"],true);
                        json!({"confirmed":ready,"signed_block":serde_json::to_string(&block).unwrap()})
                    },
                    action=>panic!("unexpected observer action {action}"),
                };
                let response=serde_json::to_string(&json!({"result":result})).unwrap();
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).as_bytes()).await.unwrap();
            }
        });
        (node,server)
    }

    #[tokio::test]
    async fn observer_waits_for_every_peer_and_records_one_confirmation() {
        use std::sync::atomic::{AtomicBool,Ordering};
        let (first,first_server)=peer(Arc::new(AtomicBool::new(true)),block()).await;
        let second_ready=Arc::new(AtomicBool::new(false));
        let (second,second_server)=peer(second_ready.clone(),block()).await;
        let directory=tempfile::tempdir().unwrap();let path=directory.path().join("report");
        let report=Arc::new(Mutex::new(OpenOptions::new().write(true).create_new(true).open(&path).unwrap()));
        let (sender,receiver)=mpsc::channel(1);sender.send(item(Instant::now()).await).await.unwrap();drop(sender);
        let nodes=[first,second];let observe=observe(receiver,&nodes,&report);tokio::pin!(observe);
        assert!(tokio::time::timeout(Duration::from_millis(75),&mut observe).await.is_err());
        assert!(!std::fs::read_to_string(&path).unwrap().contains("\"stage\":\"confirmed\""));
        second_ready.store(true,Ordering::SeqCst);
        assert_eq!(tokio::time::timeout(Duration::from_secs(2),&mut observe).await.unwrap().unwrap(),(1,1));
        let entries:Vec<Value>=std::fs::read_to_string(&path).unwrap().lines().map(|line|serde_json::from_str(line).unwrap()).collect();
        assert_eq!(entries.iter().filter(|entry|entry["stage"]=="confirmed").count(),1);
        first_server.abort();second_server.abort();let _=first_server.await;let _=second_server.await;
    }

}
