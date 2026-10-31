//! Query the default public NTS sources without changing the host clock.
//! Wait up to twenty seconds for one combined signing-time interval, then stop
//! the workers. Success is one usable observation, not sustained availability
//! or proof that every configured source responded.
use burst_network::trusted_clock::NtsClock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter("burst_network::trusted_clock=debug")
        .init();
    let service = NtsClock::new(NtsClock::public_sources())?;
    let clock = service.clock();
    let (shutdown, stop) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(service.run(stop));
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(time) = clock.reading() {
                println!(
                    "authenticated UTC interval: [{}, {}], host UTC: {}",
                    time.now,
                    time.now + time.error_bound,
                    SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs()
                );
                tokio::time::sleep(Duration::from_secs(2)).await;
                return Ok::<_, std::time::SystemTimeError>(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await;
    shutdown.send(true)?;
    task.await?;
    result??;
    Ok(())
}
