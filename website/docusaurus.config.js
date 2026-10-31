const {themes} = require('prism-react-renderer');

module.exports = {
  title: 'BURST',
  tagline: 'A currency built around people',
  url: 'https://brst.cc',
  baseUrl: '/',
  trailingSlash: true,
  onBrokenLinks: 'throw',
  markdown: {
    format: 'md',
    mermaid: true,
    hooks: {onBrokenMarkdownLinks: 'throw', onBrokenMarkdownImages: 'throw'},
  },
  presets: [['classic', {
    docs: {
      path: '../docs',
      routeBasePath: '/',
      rehypePlugins: [require('./src/plugins/currency-terms')],
      exclude: ['SUMMARY.md'],
      sidebarPath: require.resolve('./sidebars.js'),
      editUrl: 'https://github.com/BURST-UBI/protocol/edit/main/docs/',
    },
    blog: false,
    pages: false,
    theme: {customCss: require.resolve('./src/css/custom.css')},
  }]],
  themes: [
    '@docusaurus/theme-mermaid',
    ['@easyops-cn/docusaurus-search-local', {
      hashed: true,
      docsRouteBasePath: '/',
      docsDir: '../docs',
      indexBlog: false,
      highlightSearchTermsOnTargetPage: true,
    }],
  ],
  plugins: [function rawMarkdown() {
    return {
      name: 'raw-markdown',
      async postBuild({outDir}) {
        const {cp} = require('node:fs/promises');
        const {join} = require('node:path');
        await cp(join(__dirname, '../docs'), join(outDir, 'markdown'), {recursive: true});
      },
    };
  }],
  themeConfig: {
    prism: {
      theme: {
        ...themes.github,
        styles: [...themes.github.styles, {
          types: ['property', 'parameter', 'variable', 'attr-name'],
          style: {color: '#176b78'},
        }, {
          types: ['function'],
          style: {color: '#913954'},
        }],
      },
      darkTheme: themes.dracula,
      additionalLanguages: ['rust', 'toml', 'bash', 'json'],
    },
    docs: {sidebar: {autoCollapseCategories: true}},
    colorMode: {defaultMode: 'light', respectPrefersColorScheme: false},
    navbar: {
      title: 'BURST',
      items: [
        {to: '/introduction/QUICK_TOUR', label: 'Introduction', position: 'left'},
        {to: '/introduction/WHITEPAPER', label: 'Whitepaper', position: 'left'},
        {href: 'https://github.com/BURST-UBI/protocol', label: 'GitHub', position: 'right'},
      ],
    },
    footer: {
      style: 'light',
      links: [{title: 'Source', items: [
        {label: 'Protocol and documentation', href: 'https://github.com/BURST-UBI/protocol'},
        {label: 'Markdown files', href: 'https://github.com/BURST-UBI/protocol/tree/main/docs'},
      ]}],
      copyright: 'BURST · Open-source software under the MIT license',
    },
    tableOfContents: {minHeadingLevel: 2, maxHeadingLevel: 3},
  },
};
