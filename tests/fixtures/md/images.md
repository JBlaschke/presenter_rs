---
# images.md: dialect §6 (images and local assets). Relative paths resolve against the
# deck's base_dir (this directory). Behaviours:
#   - existing relative image `images/ok.png` → registered (register_file), src becomes
#     `asset:<12 hex>`; its alt, title and `{width=50%}` attributes stay on the <img>;
#   - missing relative image `images/missing.png` → `missing-asset` warning diagnostic
#     (path + span), URL left as written (Error::MissingAsset only when
#     missing_asset_is_error);
#   - remote `https://…`, protocol-relative `//…`, root-absolute `/abs.png` and `data:`
#     URLs are left untouched and never fetched;
#   - raw HTML: relative `src="…"` / `poster="…"` in HtmlBlock and HtmlInline literals are
#     rewritten the same way (`<img src="images/ok.png">`); remote ones are untouched;
#   - links (`[handout](handout.pdf)`) are not assets and stay as written;
#   - a relative `background=` slide attribute (data-background-image) is registered too;
#   - `./images/ok.png` and `../md/images/ok.png` name the same file as `images/ok.png`:
#     register_file normalizes paths lexically, so all spellings share one asset id.
# The same file registered several times yields one asset id (deck.assets lists ok.png once).
# Expected shape: 7 horizontal slides.
title: Images
---

## Local image

![A local image](images/ok.png "Local file")

![Dot-relative spelling](./images/ok.png) and ![parent-relative spelling](../md/images/ok.png)

---

## Missing image

![Missing image](images/missing.png)

---

## Remote and absolute

![Remote](https://example.org/logo.png)
![Protocol-relative](//example.org/logo.png)
![Root-absolute](/abs.png)
![Data URI](data:image/gif;base64,R0lGODlhAQABAAAAACw=)

---

## Image attributes

![Half width](images/ok.png){width=50%}

---

## Raw HTML

<img src="images/ok.png" alt="raw html block">

Inline <img src="images/ok.png" alt="raw inline" width="16"> and a video
<video src="https://example.org/clip.mp4" poster="images/ok.png"></video> with a local poster.

<img src="https://example.org/remote.png" alt="remote raw html">

---

## Links are not assets

[The handout](handout.pdf) stays a plain link.

--- {background=images/ok.png}

## Background image

The slide background is the local `images/ok.png`.
