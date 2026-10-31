import React from 'react';
import TOCItems from '@theme-original/TOCItems';

export default function CurrencyTOC(props) {
  const toc = props.toc.map(item => ({
    ...item,
    value: item.value.replace(/\b(BRN|TRST)\b/g, term =>
      `<span class="currency-term--${term.toLowerCase()}">${term}</span>`),
  }));
  return <TOCItems {...props} toc={toc} />;
}
