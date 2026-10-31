import React from 'react';
import MDXComponents from '@theme-original/MDXComponents';
import PaymentFlow from '../../components/PaymentFlow';

export default {
  ...MDXComponents,
  img: function Illustration(props) {
    if (props.src?.includes('/conversion')) return <PaymentFlow />;
    const Image = MDXComponents.img;
    if (props.title) return <span className="paper-illustration">
      <a href={props.src} target="_blank" rel="noopener noreferrer" aria-label={`Open full-size illustration: ${props.alt}`}>
        <Image {...props} title={undefined} />
      </a>
      <span className="paper-illustration__caption">{props.title}</span>
    </span>;
    return <Image {...props} />;
  },
};
