import React, {useState} from 'react';

export default function PaymentFlow() {
  const [play, setPlay] = useState(0);
  return <span className="payment-flow">
    <span className="payment-flow__drawing" key={play} data-playing={play > 0} role="img" aria-label="Burning personal BRN creates TRST for a recipient to accept with a signed receive.">
      <span className="payment-flow__balance payment-flow__balance--brn">
        <span className="payment-flow__token"><span /></span>
        <strong>BRN</strong><span>Personal allowance</span>
      </span>
      <span className="payment-flow__connection"><span>Burn to pay</span><i aria-hidden="true" /></span>
      <span className="payment-flow__balance payment-flow__balance--trst">
        <span className="payment-flow__token"><span /></span>
        <strong>TRST</strong><span>Transferable currency</span>
      </span>
    </span>
    <span className="payment-flow__caption"><span>The recipient signs to receive.</span><button type="button" onClick={() => setPlay(play + 1)}>Replay flow <span aria-hidden="true">↗</span></button></span>
  </span>;
}
