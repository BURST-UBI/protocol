import React, {useState} from 'react';
import Footer from '@theme-original/DocItem/Footer';
import {useDoc} from '@docusaurus/plugin-content-docs/client';

export default function DocFooter(props) {
  const {metadata} = useDoc();
  const [status, setStatus] = useState('');
  const url = `/markdown/${metadata.id}.md`;
  async function copyMarkdown() {
    try {
      const response = await fetch(url);
      if (!response.ok) throw new Error('Markdown unavailable');
      await navigator.clipboard.writeText(await response.text());
      setStatus('Copied');
    } catch {
      setStatus('Could not copy. Open the Markdown link instead.');
    }
  }
  return <>
    <div className="margin-vert--lg">
      <a href={url}>View Markdown</a>
      <button className="button button--secondary button--sm margin-left--md" onClick={copyMarkdown}>Copy Markdown</button>
      <span role="status" className="margin-left--md">{status}</span>
    </div>
    <Footer {...props} />
  </>;
}
