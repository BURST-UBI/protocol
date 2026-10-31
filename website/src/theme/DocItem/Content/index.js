import React from 'react';
import Content from '@theme-original/DocItem/Content';
import {useDoc} from '@docusaurus/plugin-content-docs/client';

export default function DocContent(props) {
  const {metadata} = useDoc();
  return <div className={metadata.id === 'README' ? 'docs-home' : undefined}>
    <Content {...props} />
  </div>;
}
