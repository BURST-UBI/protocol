# BURST website

Docusaurus renders `../docs` directly. Edit those Markdown files for content and
`../docs/SUMMARY.md` for navigation. There is no second documentation copy.
Mermaid diagrams and local search use the same build. Raw Markdown is served
under `/markdown/`.

Use Node.js 22 or newer. From this directory:

```sh
npm ci
npm start
npm run build
npm run serve
```

On the development desktop, run these commands through `../tools/limited-run.sh`
as required by the workspace resource policy.

## Vercel

Import `BURST-UBI/protocol`, use the repository root (not `website`) as the project
root, and select the Other framework preset. The root `vercel.json` specifies
installation, build and output paths. Set Node.js 22 in the project settings.
Connect `main` for production; pull requests receive preview deployments.
Every deployment reads docs and website code from the same commit.

Check a preview before assigning `brst.cc` to this project. The existing static
export in the whitepaper repository remains the previous site until cutover.
Vercel's Hobby tier supports custom domains but is limited to personal,
non-commercial use; it is not a blanket free commercial-hosting plan.
