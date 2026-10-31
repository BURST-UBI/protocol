const excluded = new Set(['pre', 'code', 'script', 'style', 'svg', 'math']);

module.exports = function currencyTerms() {
  return tree => {
    function visit(node) {
      if (!node.children || excluded.has(node.tagName)) return;
      node.children = node.children.flatMap(child => {
        if (child.type !== 'text') {
          visit(child);
          return [child];
        }
        return child.value.split(/\b(BRN|TRST)\b/g).filter(Boolean).map(value =>
          value === 'BRN' || value === 'TRST'
            ? {type: 'element', tagName: 'span', properties: {className: [`currency-term--${value.toLowerCase()}`]}, children: [{type: 'text', value}]}
            : {type: 'text', value},
        );
      });
    }
    visit(tree);
  };
};
