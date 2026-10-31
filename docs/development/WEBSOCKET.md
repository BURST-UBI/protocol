# WebSocket events

Use WebSocket events to refresh an interface when the node processes a block.
Use RPC to recover state after a disconnect and to check an operation's outcome.
The event stream has no replay or delivery guarantee.

## Connect and subscribe

Enable the listener with `--websocket`. Its default address is
`ws://127.0.0.1:7078/ws`. For a browser served over HTTPS, expose it through a
properly secured `wss://` endpoint.

This browser example logs confirmations for all accounts:

```javascript
const socket = new WebSocket('ws://127.0.0.1:7078/ws');
socket.addEventListener('open', () => {
  socket.send(JSON.stringify({ action: 'subscribe', topic: 'confirmation' }));
});
socket.addEventListener('message', ({ data }) => {
  const message = JSON.parse(data);
  if (message.topic === 'confirmation') {
    console.log(message.data.block_hash, message.data.account);
  } else if (message.type === 'error') {
    console.error(message.message);
  }
});
```

The server acknowledges the subscription:

```json
{"type":"ack","action":"subscribe","topic":"confirmation"}
```

To restrict it to an account, add a filter containing real BURST addresses:

```json
{"action":"subscribe","topic":"confirmation","filter":{"accounts":["YOUR_ACCOUNT_ADDRESS"]}}
```

The filter matches the block's `data.account`. It does not match every participant
in a payment: filtering for a recipient will not capture a sender's block merely
because it pays that recipient. Omit `accounts` for all accounts; an empty array
matches none. Subscribing again replaces the existing filter for that topic.
Block-type and minimum-amount filters are not supported.

## Event fields

Events have `topic`, `data` and `timestamp` at the top level. They do not have the
`type` field used by acknowledgments. This is an illustrative confirmation event;
the address and hash are placeholders:

```json
{
  "topic": "confirmation",
  "data": {
    "account": "ACCOUNT_ADDRESS",
    "block_hash": "BLOCK_HASH",
    "amount": "2000000000000000000"
  },
  "timestamp": 1700000000
}
```

`amount` is the block's TRST balance, **not the amount transferred**. Preserve it
as an integer string. `timestamp` is the publisher's Unix time in seconds, not
the signed block timestamp.

| Topic | When it is sent | Data fields |
| --- | --- | --- |
| `confirmation` | The node cements a block as confirmed. | `account`, `block_hash`, `amount` |
| `account_update` | The node accepts a block, before confirmation. | `account`, `balance`, `change_type` |
| `governance` | No node lifecycle publisher is connected yet. | Do not rely on it. |
| `verification` | No node lifecycle publisher is connected yet. | Do not rely on it. |

Treat `account_update` as provisional. For a payment notification, confirm the
specific block and inspect its operation; do not infer a payment from a changed
balance alone.

## Reconnect and reconcile

On each new connection, subscribe again and query account state through
[RPC](RPC.md). Also refresh periodically while connected: a slow consumer can
miss messages without receiving a gap notification. Track submitted actions by
their block hashes, not by whether an event arrived.

To unsubscribe or check that the connection responds:

```json
{"action":"unsubscribe","topic":"confirmation"}
```

```json
{"action":"ping"}
```

A ping returns `{"type":"pong"}`. Listener limits and deployment settings are
documented under [RPC and networking](../node/SERVICES.md).

Source: [subscription messages and filters](https://github.com/BURST-UBI/protocol/blob/main/websocket/src/subscriptions.rs),
[event payloads](https://github.com/BURST-UBI/protocol/blob/main/websocket/src/state.rs),
[node publishers](https://github.com/BURST-UBI/protocol/blob/main/node/src/node.rs).
