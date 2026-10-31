import React from 'react';

export default function PaymentFlow() {
  return <span className="payment-flow" role="group" aria-label="Alice pays Bob 50 TRST using her BRN">
    <span className="payment-flow__steps" role="list">
      <span className="payment-flow__step" role="listitem">
        <span className="payment-flow__number">01</span>
        <strong>Alice signs the payment</strong>
        <span className="payment-flow__amount currency-term--brn">50 BRN spent</span>
        <span>Her BRN is burned. This creates an equal amount of TRST for Bob.</span>
      </span>
      <span className="payment-flow__step" role="listitem">
        <span className="payment-flow__number">02</span>
        <strong>The payment waits</strong>
        <span className="payment-flow__amount currency-term--trst">50 TRST pending</span>
        <span>Bob has not accepted it yet. Neither person can spend this pending payment.</span>
      </span>
      <span className="payment-flow__step" role="listitem">
        <span className="payment-flow__number">03</span>
        <strong>Bob signs to accept</strong>
        <span className="payment-flow__amount currency-term--trst">50 TRST received</span>
        <span>Once confirmed, Bob can use this TRST to pay someone else.</span>
      </span>
    </span>
  </span>;
}
