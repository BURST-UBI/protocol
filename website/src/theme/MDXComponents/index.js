import React from 'react';
import MDXComponents from '@theme-original/MDXComponents';
import PaymentFlow from '../../components/PaymentFlow';

export default {
  ...MDXComponents,
  img: function Illustration(props) {
    if (props.src?.includes('/conversion')) return <PaymentFlow />;
    const Image = MDXComponents.img;
    return <Image {...props} />;
  },
};
