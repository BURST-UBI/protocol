import React from 'react';
import Content from '@theme-original/DocItem/Content';
import {useDoc} from '@docusaurus/plugin-content-docs/client';

export default function DocContent(props) {
  const {metadata} = useDoc();
  const whitepaper = metadata.id === 'introduction/WHITEPAPER';
  const introduction = metadata.id === 'introduction/QUICK_TOUR' || metadata.id === 'README';
  return <div className={whitepaper ? 'paper paper--whitepaper' : introduction ? 'paper' : undefined}>
    {whitepaper && <div className="paper__masthead">
      <span>Whitepaper</span>
      <span className="paper__mark" aria-hidden="true"><i /><i /></span>
    </div>}
    <Content {...props} />
  </div>;
}
