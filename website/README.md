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

## Protocol defaults

`ProtocolParams` and the existing governance parameter registry supply the values
in `protocol-defaults.json`. Markdown keeps readable numbers between
`<!-- default:live.trst_expiry_secs:years -->70<!-- /default -->` markers.
Use `live`, `test` or `dev` and the exact parameter field name. The default format
uses the registry's unit; `raw`, `number`, `years`, `percent`, `hourly` and
`hourly_approx` are available for prose. Years mean 365 days.

After changing Rust defaults, run from the repository root:

```sh
bash tools/limited-run.sh node tools/documentation-defaults.mjs --write
```

This evaluates the Rust defaults, updates marked Markdown values and refreshes
coverage entries without carrying implementation sign-offs onto changed text.
Commit the JSON and Markdown together. Examples using hypothetical settings stay
literal; explanatory prose still needs review when a policy changes.

Website builds reject changed source fingerprints or stale marked values.
CI additionally evaluates Rust and compares the exported values. Vercel needs
only Node.js. These are code defaults, not a running network's governed state.
