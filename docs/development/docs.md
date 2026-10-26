# Writing documentation

This site is built with [MkDocs](https://www.mkdocs.org/) and the Material theme. Sources are in `docs/`, the navigation in `mkdocs.yml`.

## Preview locally

```bash
python3 -m venv .venv
.venv/bin/pip install -r requirements-docs.txt
.venv/bin/mkdocs serve
```

Then open <http://127.0.0.1:8000>. `mkdocs build --strict` fails on broken links, as CI does.

## Publishing

| Host | How |
|---|---|
| GitLab Pages | `.gitlab-ci.yml` job `pages` builds the site into `public/` on the default branch. |
| GitHub Pages | `.github/workflows/docs.yml` builds and deploys on pushes to `main`/`master`. In the repository settings set **Pages → Source** to **GitHub Actions** once. |

Merge requests / pull requests build the site with `--strict` without publishing it.

## Conventions

- One page per module under `docs/features/`, with: what it does, how to use it, a settings table (JSON key, default, meaning), how it works, limitations.
- Config keys are written exactly as in JSON (camelCase).
- When a module's defaults or shortcuts change, update its page and the table in `docs/features/index.md` in the same change.
