#!/usr/bin/env python3
"""Build the project website: the rendered examples and a landing page.

Usage:
  scripts/site.py [--presenter PATH] [--out DIR] [--repo OWNER/NAME] [--sha SHA] [--ref NAME]
                  [--release-json FILE]

.github/workflows/site.yml runs this on every push to main and on every published release
and deploys the output to GitHub Pages (docs/dev/site.md). Locally:

  cargo build --release && scripts/site.py && python3 -m http.server -d dist/site

What it does:
  1. Finds the examples: every committed examples/<name>/slides.md, or the one committed
     .ipynb of an example that has no slides.md. The list comes from `git ls-files`, so an
     example that is only on disk is never published.
  2. Runs `presenter check --deny-warnings` on each, builds each into
     <out>/examples/<name>/index.html (one self-contained file) and reads its title and
     subtitle with `presenter dump`.
  3. Writes <out>/index.html from docs/site/index.html (a string.Template): the download
     table of the latest release (from --release-json, the GitHub API's `releases/latest`
     document; without it, a note that there is no release yet), the example cards, the
     guide links and a build stamp. Copies docs/site/site.css and the screenshots the page
     shows next to it.

Needs python3, git and a presenter binary (default: target/release/presenter, then
target/debug/presenter, then `presenter` on PATH). The output directory is emptied first;
to be safe, the script only empties a directory it wrote before (it leaves a marker file),
or an empty one.
"""

from __future__ import annotations

import argparse
import html
import json
import os
import re
import shutil
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
from string import Template

ROOT = Path(__file__).resolve().parent.parent
TEMPLATE_DIR = ROOT / "docs" / "site"
SCREENSHOT_DIR = ROOT / "docs" / "screenshots"

# Written into the output directory; the script empties only a directory that has it.
MARKER = ".presenter-site"

# The release targets (.github/workflows/release.yml), in the order of the download table:
# target triple, platform, and what the archive is for.
TARGETS = [
    ("aarch64-apple-darwin", "macOS", "Apple silicon"),
    ("x86_64-apple-darwin", "macOS", "Intel"),
    ("x86_64-unknown-linux-gnu", "Linux", "x86_64, glibc"),
    ("aarch64-unknown-linux-gnu", "Linux", "arm64, glibc"),
    ("x86_64-pc-windows-msvc", "Windows", "x64"),
    ("aarch64-pc-windows-msvc", "Windows", "arm64"),
]

# The user guide's pages in reading order (README.md's order). Pages not listed here are
# appended alphabetically, so a new page is never missing from the site.
GUIDE_ORDER = [
    "getting-started.md",
    "authoring.md",
    "notebooks.md",
    "themes.md",
    "configuration.md",
    "cli.md",
]

# docs/screenshots files the landing page shows (template placeholder -> file).
SCREENSHOTS = {
    "hero": "title.png",
    "shot_code": "code.png",
    "shot_notebook": "notebook-code-plot.png",
}

# `presenter build` reports what it wrote: `Wrote dist/slides.html (17 slides, 2.5 MB)`.
WROTE = re.compile(r"\((\d+) slides?, ([0-9.]+ [kMG]?B)\)\s*$")


def main() -> int:
    args = parse_args()
    presenter = find_presenter(args.presenter)
    repo = args.repo
    sha = args.sha or git("rev-parse", "HEAD")
    link_ref = args.ref if args.ref and "/" not in args.ref else sha
    out = Path(args.out).resolve()

    prepare_output(out)
    print(f"presenter: {presenter} ({run([presenter, '--version']).strip()})")
    print(f"site: {out}")

    cards = []
    for name, entry in examples():
        cards.append(build_example(presenter, name, entry, out))

    release = None
    if args.release_json:
        with open(args.release_json, encoding="utf-8") as file:
            release = json.load(file)

    template = Template((TEMPLATE_DIR / "index.html").read_text(encoding="utf-8"))
    page = template.substitute(
        repo=html.escape(repo),
        repo_url=html.escape(f"https://github.com/{repo}"),
        releases_url=html.escape(f"https://github.com/{repo}/releases"),
        download=download_section(release, repo),
        examples=examples_section(cards, repo, link_ref),
        guide=guide_section(repo, link_ref),
        stamp=stamp(repo, sha, args.ref),
        **{key: html.escape(f"screenshots/{file}") for key, file in SCREENSHOTS.items()},
    )
    (out / "index.html").write_text(page, encoding="utf-8")
    shutil.copy(TEMPLATE_DIR / "site.css", out / "site.css")
    (out / "screenshots").mkdir()
    for file in SCREENSHOTS.values():
        shutil.copy(SCREENSHOT_DIR / file, out / "screenshots" / file)
    # GitHub Pages serves the directory as it is; this file is the convention that says so.
    (out / ".nojekyll").write_text("", encoding="utf-8")

    total = sum(path.stat().st_size for path in out.rglob("*") if path.is_file())
    print(f"wrote {out / 'index.html'}: {len(cards)} examples, {total / 1_000_000:.1f} MB in all")
    return 0


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n", 1)[0])
    parser.add_argument("--presenter", help="the presenter binary (default: the newest build in target/, then PATH)")
    parser.add_argument("--out", default=str(ROOT / "dist" / "site"), help="output directory (default: dist/site)")
    parser.add_argument("--repo", default="JBlaschke/presenter_rs", help="GitHub repository, OWNER/NAME")
    parser.add_argument("--sha", help="the commit the site is built from (default: git rev-parse HEAD)")
    parser.add_argument("--ref", help="the branch or tag the site is built from, for the links into the repository")
    parser.add_argument("--release-json", help="the GitHub API's releases/latest document, for the download table")
    return parser.parse_args()


def find_presenter(explicit: str | None) -> str:
    if explicit:
        return explicit
    for profile in ("release", "debug"):
        for name in ("presenter", "presenter.exe"):
            candidate = ROOT / "target" / profile / name
            if candidate.is_file():
                return str(candidate)
    found = shutil.which("presenter")
    if not found:
        sys.exit("error: no presenter binary: run `cargo build --release` or pass --presenter")
    return found


def prepare_output(out: Path) -> None:
    if out.exists():
        if not out.is_dir():
            sys.exit(f"error: {out} is not a directory")
        if any(out.iterdir()) and not (out / MARKER).is_file():
            sys.exit(f"error: {out} is not empty and was not written by this script (no {MARKER}); choose another --out")
        shutil.rmtree(out)
    out.mkdir(parents=True)
    (out / MARKER).write_text("written by scripts/site.py; the script empties this directory\n", encoding="utf-8")


def examples() -> list[tuple[str, Path]]:
    """The committed examples: (name, entry file relative to the repository root)."""
    listed = git("ls-files", "-z", "--", "examples/*/slides.md", "examples/*/*.ipynb").split("\0")
    by_example: dict[str, list[Path]] = {}
    for line in listed:
        if line:
            path = Path(line)
            by_example.setdefault(path.parts[1], []).append(path)
    found = []
    for name in sorted(by_example):
        entries = by_example[name]
        decks = [path for path in entries if path.name == "slides.md"]
        if not decks:
            decks = [path for path in entries if path.suffix == ".ipynb"]
        if len(decks) != 1:
            names = ", ".join(str(path) for path in decks)
            sys.exit(f"error: examples/{name}: expected one slides.md or one notebook, found: {names}")
        found.append((name, decks[0]))
    if not found:
        sys.exit("error: no examples found (git ls-files examples/*/slides.md examples/*/*.ipynb)")
    return found


def build_example(presenter: str, name: str, entry: Path, out: Path) -> dict:
    target = out / "examples" / name / "index.html"
    run([presenter, "check", "--deny-warnings", str(entry)])
    wrote = run([presenter, "build", str(entry), "-o", str(target)]).strip().splitlines()[-1]
    match = WROTE.search(wrote)
    if not match:
        sys.exit(f"error: examples/{name}: unexpected build output: {wrote!r}")
    meta = json.loads(run([presenter, "dump", str(entry)]))["meta"]
    print(f"examples/{name}: {wrote}")
    readme = ROOT / "examples" / name / "README.md"
    return {
        "name": name,
        "entry": entry.as_posix(),
        "title": meta.get("title") or name,
        # What the example shows: its README's first paragraph, else the deck's subtitle.
        "description": readme_summary(readme) or meta.get("subtitle") or meta.get("description") or "",
        "slides": int(match.group(1)),
        "size": match.group(2),
        "readme": readme.is_file(),
    }


def readme_summary(readme: Path) -> str:
    """The first paragraph after the title of an example's README, as plain text."""
    if not readme.is_file():
        return ""
    paragraphs = re.split(r"\n\s*\n", readme.read_text(encoding="utf-8").strip())
    for paragraph in paragraphs:
        if paragraph.startswith("#"):
            continue
        text = " ".join(paragraph.split())
        # [text](url) -> text, marked; then drop the parentheses that only pointed somewhere
        # ("(docs/spec/…)", "(see …)"), markers and all; then the code ticks.
        text = re.sub(r"!?\[([^\]]*)\]\([^)]*\)", "\0\\1\0", text)
        text = re.sub(r"\s*\([^()]*\0[^()]*\)", "", text)
        text = text.replace("\0", "").replace("`", "")
        return " ".join(text.split())
    return ""


def download_section(release: dict | None, repo: str) -> str:
    releases_url = html.escape(f"https://github.com/{repo}/releases")
    if not release or not release.get("assets"):
        return (
            "<p>No release has been published yet. Build from source with Rust 1.89 or newer:</p>\n"
            "<pre><code>git clone " + html.escape(f"https://github.com/{repo}.git") + "\n"
            "cd " + html.escape(repo.split("/")[-1]) + "\n"
            "cargo install --path .</code></pre>\n"
            f'<p>Releases will appear on the <a href="{releases_url}">releases page</a>.</p>\n'
        )
    tag = release["tag_name"]
    assets = {asset["name"]: asset for asset in release["assets"]}
    rows = []
    for triple, platform, what in TARGETS:
        found = [asset for name, asset in assets.items() if name.startswith(f"presenter-{tag}-{triple}.")]
        if len(found) != 1:
            continue
        asset = found[0]
        rows.append(
            "<tr>"
            f"<td>{html.escape(platform)}</td><td>{html.escape(what)}</td>"
            f'<td><a href="{html.escape(asset["browser_download_url"])}"><code>{html.escape(asset["name"])}</code></a></td>'
            f"<td class=\"num\">{asset['size'] / 1_000_000:.1f} MB</td>"
            "</tr>"
        )
    date = (release.get("published_at") or "")[:10]
    parts = [
        f'<p class="release"><strong>Latest release: <a href="{html.escape(release["html_url"])}">presenter {html.escape(tag)}</a></strong>'
        + (f", published {html.escape(date)}" if date else "")
        + ".</p>\n"
    ]
    if rows:
        parts.append(
            "<table class=\"downloads\">\n<thead><tr><th>Platform</th><th></th><th>Archive</th><th>Size</th></tr></thead>\n<tbody>\n"
            + "\n".join(rows)
            + "\n</tbody>\n</table>\n"
        )
    sums = assets.get("SHA256SUMS")
    notes = []
    if sums:
        notes.append(
            f'Check a download against <a href="{html.escape(sums["browser_download_url"])}"><code>SHA256SUMS</code></a>, '
            "then unpack it (<code>tar -xzf</code>, or unzip on Windows) and put <code>presenter</code> on your <code>PATH</code>."
        )
    notes.append(
        "The binaries are not signed: on macOS, remove the quarantine mark of a browser download with "
        "<code>xattr -d com.apple.quarantine presenter</code>."
    )
    notes.append("PDF and PowerPoint export need Chrome or Chromium, and are not available on Windows yet.")
    parts.append("<ul class=\"notes\">\n" + "\n".join(f"<li>{note}</li>" for note in notes) + "\n</ul>\n")
    parts.append(f'<p><a href="{releases_url}">All releases and their notes</a>.</p>\n')
    return "".join(parts)


def examples_section(cards: list[dict], repo: str, ref: str) -> str:
    items = []
    for card in cards:
        name = html.escape(card["name"])
        source = f"https://github.com/{repo}/tree/{ref}/examples/{card['name']}"
        if card["readme"]:
            source = f"https://github.com/{repo}/blob/{ref}/examples/{card['name']}/README.md"
        description = f'<p class="description">{html.escape(card["description"])}</p>\n' if card["description"] else ""
        items.append(
            '<article class="card">\n'
            f'<p class="kicker">examples/{name}</p>\n'
            f'<h3><a href="examples/{name}/">{html.escape(card["title"])}</a></h3>\n'
            + description
            + f'<p class="meta">{card["slides"]} slides, one {html.escape(card["size"])} file</p>\n'
            f"<pre><code>presenter build {html.escape(card['entry'])}</code></pre>\n"
            '<p class="links">'
            f'<a class="button" href="examples/{name}/">Open the deck</a> '
            f'<a href="{html.escape(source)}">Source</a>'
            "</p>\n"
            "</article>"
        )
    return "\n".join(items) + "\n"


def guide_section(repo: str, ref: str) -> str:
    guide_dir = ROOT / "docs" / "guide"
    pages = [name for name in GUIDE_ORDER if (guide_dir / name).is_file()]
    pages += sorted(
        path.name for path in guide_dir.glob("*.md") if path.name not in pages and path.name != "README.md"
    )
    items = []
    for name in pages:
        title = name
        for line in (guide_dir / name).read_text(encoding="utf-8").splitlines():
            if line.startswith("# "):
                title = line[2:].strip()
                break
        url = f"https://github.com/{repo}/blob/{ref}/docs/guide/{name}"
        items.append(f'<li><a href="{html.escape(url)}">{html.escape(title)}</a></li>')
    return "<ul>\n" + "\n".join(items) + "\n</ul>\n"


def stamp(repo: str, sha: str, ref: str | None) -> str:
    date = datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M UTC")
    commit = f'<a href="{html.escape(f"https://github.com/{repo}/commit/{sha}")}"><code>{html.escape(sha[:7])}</code></a>'
    where = f" ({html.escape(ref)})" if ref else ""
    return f"Built from commit {commit}{where} on {html.escape(date)}."


def git(*args: str) -> str:
    return run(["git", "-C", str(ROOT), *args])


def run(command: list[str]) -> str:
    result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, encoding="utf-8")
    if result.returncode != 0:
        sys.stderr.write(result.stderr)
        sys.exit(f"error: {' '.join(command)} exited with {result.returncode}")
    if result.stderr.strip():
        sys.stderr.write(result.stderr)
    return result.stdout


if __name__ == "__main__":
    sys.exit(main())
