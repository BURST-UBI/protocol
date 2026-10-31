import React, {useState} from 'react';

export default function PaymentFlow() {
  const [play, setPlay] = useState(0);
  return <span className="payment-flow">
    <span className="payment-flow__drawing" key={play} data-playing={play > 0} role="img" aria-label="You spend BRN to create a payment in TRST. The person you pay signs to accept it.">
      <span className="payment-flow__balance payment-flow__balance--brn">
        <span className="payment-flow__token"><span /></span>
        <strong>BRN</strong><span>Personal income</span>
      </span>
      <span className="payment-flow__connection"><span>Spend BRN</span><i aria-hidden="true" /></span>
      <span className="payment-flow__balance payment-flow__balance--trst">
        <span className="payment-flow__token"><span /></span>
        <strong>TRST</strong><span>Money you can pass on</span>
      </span>
    </span>
    <span className="payment-flow__caption"><span>The person you pay signs to accept the payment.</span><button type="button" onClick={() => setPlay(play + 1)}>Play animation</button></span>
  </span>;
}
