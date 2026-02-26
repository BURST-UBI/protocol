# BURST Testnet Setup & Troubleshooting

## Node roles

| Role    | bootstrap_peers | enable_faucet | Notes                          |
|---------|-----------------|---------------|--------------------------------|
| **Seed**  | `[]`            | `true`        | Creates genesis, sources blocks |
| **Non-seed** | `["SEED_IP:17076"]` | `false` | Bootstraps from seed, meshes with others |

**Advertise address:** On cloud VPSes (DigitalOcean, etc.) UPnP fails. The node now auto-detects its public IP at startup (no per-node config needed). CI can deploy the same binary/config to all nodes.

## Installing

### Seed node (e.g. 167.172.83.88)

```bash
BURST_IS_SEED=1 curl -fsSL ... | sudo sh
```

### Non-seed nodes (e.g. nodes 2–5)

```bash
curl -fsSL ... | sudo sh
```

(Default `BURST_SEED` is 167.172.83.88:17076. Optional: `BURST_ADVERTISE_ADDRESS` to override auto-detect if needed.)

---

## Fixing empty ledgers (0 blocks, 0 accounts)

If telemetry shows `block_count: 0` and `account_count: 0`, the node did not create or sync genesis.

### 1. Ensure seed has genesis

On the **seed node** only:

1. Stop the daemon:
   ```bash
   sudo systemctl stop burst-node
   ```

2. Clear the data directory (to force genesis creation on next start):
   ```bash
   sudo rm -rf /var/lib/burst/*
   ```

3. Confirm config:
   ```bash
   cat /etc/burst/config.toml | grep -E "bootstrap_peers|enable_faucet"
   ```
   Expect: `bootstrap_peers = []` and `enable_faucet = true`.

4. Start:
   ```bash
   sudo systemctl start burst-node
   ```

5. Verify genesis:
   ```bash
   curl -s -X POST http://localhost:7077 -H "Content-Type: application/json" \
     -d '{"action":"node_info"}' | jq '.result.block_count, .result.account_count'
   ```
   Should show `1` and `1`.

### 2. Non-seed nodes: bootstrap from seed

On each non-seed node:

1. Ensure config has the seed (and optionally all nodes for full mesh):
   ```bash
   grep bootstrap_peers /etc/burst/config.toml
   ```
   Minimum: `bootstrap_peers = ["167.172.83.88:17076"]`.
   For full mesh (each node connects to all others), add all:
   ```toml
   bootstrap_peers = ["167.172.83.88:17076", "159.65.80.231:17076", "143.244.131.5:17076", "157.230.164.117:17076", "164.92.242.73:17076"]
   ```

2. Optionally reset and resync:
   ```bash
   sudo systemctl stop burst-node
   sudo rm -rf /var/lib/burst/*
   sudo systemctl start burst-node
   ```

3. Check that the node connects to the seed (P2P port 17076 must be reachable):
   ```bash
   curl -s -X POST http://localhost:7077 -H "Content-Type: application/json" \
     -d '{"action":"peers"}' | jq .
   ```

---

## Health check

```bash
./deploy/health-check.sh 167.172.83.88 159.65.80.231 143.244.131.5 157.230.164.117 164.92.242.73
```
