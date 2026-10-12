# The project website

The website at <https://jblaschke.github.io/presenter_rs/> is a GitHub Pages site built by
[`.github/workflows/site.yml`](../../.github/workflows/site.yml). It has one landing page
with the downloads of the latest release, links into the user guide, and every example deck
rendered by the `presenter` of the same commit.

| Path on the site | What it is |
|---|---|
| `index.html`, `site.css` | The landing page, from [`docs/site/index.html`](../site/index.html) and [`docs/site/site.css`](../site/site.css) |
| `screenshots/*.png` | The pictures the page shows, copied from `docs/screenshots/` |
| `examples/<name>/index.html` | Each example as one self-contained HTML file (`presenter build`), so `examples/basic/` opens the basic deck |

## How it is built

[`scripts/site.py`](../../scripts/site.py) does the work, so the same site can be built
locally:

```sh
cargo build --release
scripts/site.py                       # writes dist/site/ (git-ignored)
python3 -m http.server -d dist/site   # http://localhost:8000/
```

The script

1. lists the examples with `git ls-files`: every committed `examples/<name>/slides.md`, or
   the one committed `.ipynb` of an example without a `slides.md`. An example that exists
   only on disk is never published, and a new committed example appears on the site with
   no further change;
2. runs `presenter check --deny-warnings` on each example, so a warning in an example fails
   the build instead of going live, then `presenter build … -o <site>/examples/<name>/index.html`
   and `presenter dump` for the title and subtitle on the example's card;
3. fills the template with the download table, the cards, the guide links (the `# ` heading
   of each `docs/guide/*.md` page, in the order of `GUIDE_ORDER` in the script) and a stamp
   naming the commit, and copies the stylesheet and screenshots.

The download table comes from the GitHub API's `releases/latest` document, which the
workflow fetches with `gh api` and passes as `--release-json`; the script maps each
`presenter-<tag>-<target>.<ext>` asset to a platform. Without the file, or before the first
release, the page says that there is no release yet and shows the from-source command. The
script needs only python3, git and a `presenter` binary (`--presenter`; by default the newest
build under `target/`). It empties its output directory first, but only one it wrote before
(it leaves a `.presenter-site` marker) or an empty one.

## When it runs

| Event | What happens |
|---|---|
| Push to `main` | Build and deploy. The examples and the guide links follow `main`. |
| `release.yml` has published a release | Its last step asks for a "Run workflow" run on `main`, so the download table shows the new release. A run on `main` rather than the `release: published` event, because GitHub starts no workflow for an event that a workflow's own token raised, and rather than on the tag, because the `github-pages` environment deploys from `main` only (Settings → Environments); a release tag is on `main`, so the run builds the tagged commit. A release published by hand needs a "Run workflow" run too. |
| Pull request | Build only. The site is the run's `github-pages` artifact, for a look at a change; nothing is deployed. |
| "Run workflow" on the Actions tab | Build and deploy from the chosen branch. |

The `build` job compiles `presenter` in release mode (embedded assets, as shipped), runs the
script and uploads the directory with `actions/upload-pages-artifact`; the `deploy` job
publishes it with `actions/deploy-pages` through the `github-pages` environment. Deployments
share one concurrency group (`pages`), so two deployments never overlap; a later one waits.

## One-time setup

GitHub Pages must be told to take its content from Actions, once, by a repository admin:
Settings → Pages → "Build and deployment" → Source: **GitHub Actions**. Or with the CLI:

```sh
gh api -X POST repos/JBlaschke/presenter_rs/pages -f build_type=workflow
```

Until then the `configure-pages` step fails with "Get Pages site failed", and nothing is
deployed. Pull-request builds do not need the setting.

## Changing the page

- Copy and layout: edit `docs/site/index.html` and `docs/site/site.css`. The template is a
  Python `string.Template`, so a placeholder is `${name}` and a literal dollar sign is `$$`.
  The placeholders are `repo`, `repo_url`, `releases_url`, `download`, `examples`, `guide`,
  `stamp`, and one per screenshot (`hero`, `shot_code`, `shot_notebook`).
- Another screenshot: add it to `SCREENSHOTS` in the script and use the new placeholder.
- A new release target: add its triple to `TARGETS` in the script, next to the matrix entry
  in `release.yml`, or it is left out of the download table.
- The page loads no fonts, scripts or styles from elsewhere, so it works offline and on an
  air-gapped mirror, like the decks it links to. Keep it that way.
