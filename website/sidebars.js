const {readFileSync} = require('node:fs');
const {join} = require('node:path');

const items = [];
let group;
for (const line of readFileSync(join(__dirname, '../docs/SUMMARY.md'), 'utf8').split('\n')) {
  if (line.startsWith('## ')) {
    group = {type: 'category', label: line.slice(3), collapsed: true, items: []};
    items.push(group);
  } else {
    const match = line.match(/^\* \[([^\]]+)\]\(([^)]+)\.md\)$/);
    if (match) {
      const item = {type: 'doc', id: match[2], label: match[1]};
      (group ? group.items : items).push(item);
    }
  }
}
module.exports = {docs: items};
