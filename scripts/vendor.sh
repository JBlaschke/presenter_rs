#!/usr/bin/env bash
# Re-download and trim the vendored front-end packages (docs/spec/vendored-assets.md).
#
# Usage:
#   scripts/vendor.sh            # run from anywhere; writes into <repo>/assets/vendor/
#
# What it does:
#   1. downloads the pinned npm tarballs of reveal.js and KaTeX, and of the libraries reveal
#      bundles into its files (highlight.js, highlightjs-line-numbers.js, marked and fitty;
#      only their license files are kept), into a temporary directory,
#   2. verifies each tarball's SHA-256 against the value hard-coded below (aborts on mismatch),
#   3. copies exactly the files listed in docs/spec/vendored-assets.md §2 (nothing else)
#      into a staging directory, completes League Gothic's license notice, and writes
#      VERSIONS.toml there,
#   4. only if every step succeeded, replaces assets/vendor/ with the staging directory
#      (a failure leaves the existing assets/vendor/ untouched).
#
# The output depends only on the pinned tarballs, so running it twice produces no git diff
# (CI checks this). Requirements: bash, curl, tar, awk, and sha256sum or shasum.
#
# Upgrading (vendored-assets.md §5): edit the versions, URLs, hashes and PINNED_DATE below
# (the bundled libraries: the versions the new reveal.js release was built with, see the
# comment above their pins), run the script, review `git diff`, then run `cargo test` and the
# browser smoke test.

set -euo pipefail

REVEAL_VERSION="6.0.2"
REVEAL_URL="https://registry.npmjs.org/reveal.js/-/reveal.js-${REVEAL_VERSION}.tgz"
REVEAL_SHA256="2f0c20bfb02447a198a1ebe8a8c097a5437d035caa38f2820bf4ba887a3eac91"

KATEX_VERSION="0.19.0"
KATEX_URL="https://registry.npmjs.org/katex/-/katex-${KATEX_VERSION}.tgz"
KATEX_SHA256="d8e49f2fea6eeed7cdae2cf4fd48e2f1d348758aa9e5740715e4533c2c96d1ee"

# Libraries reveal.js bundles into its own files. reveal's tarball carries none of their
# licenses, so they come from the libraries' own npm packages (only the license files are
# kept). The versions are the ones reveal 6.0.2 was built with: reveal's package-lock.json at
# the 6.0.2 tag (https://raw.githubusercontent.com/hakimel/reveal.js/6.0.2/package-lock.json,
# `packages["node_modules/<name>"].version`) for the npm dependencies, and for
# highlightjs-line-numbers.js, which reveal copies into its source, the version in the header
# comment of plugin/highlight/plugin.js at that tag. Upgrade these together with reveal.
#   dist/plugin/highlight.js        highlight.js (dist/plugin/highlight/*.css are its styles)
#                                   and highlightjs-line-numbers.js
#   dist/plugin/notes.js            marked
#   dist/reveal.js                  fitty
HLJS_VERSION="11.11.1"
HLJS_URL="https://registry.npmjs.org/highlight.js/-/highlight.js-${HLJS_VERSION}.tgz"
HLJS_SHA256="fbc9443263e976583bd5640e550b77f84d8d7e19fc8408dfe53776a5bae7ed39"

HLJS_LN_VERSION="2.8.0"
HLJS_LN_URL="https://registry.npmjs.org/highlightjs-line-numbers.js/-/highlightjs-line-numbers.js-${HLJS_LN_VERSION}.tgz"
HLJS_LN_SHA256="828745455b960fde2e8bf835e6a6b424706c46fb99b1f6a9af220935d49633b3"

MARKED_VERSION="17.0.5"
MARKED_URL="https://registry.npmjs.org/marked/-/marked-${MARKED_VERSION}.tgz"
MARKED_SHA256="1aebca0400d160cb1bedca2e271daad0e324503b7e0d0b4b23781ca3293bacce"

FITTY_VERSION="2.4.2"
FITTY_URL="https://registry.npmjs.org/fitty/-/fitty-${FITTY_VERSION}.tgz"
FITTY_SHA256="d5b57f6687fc924cd6fb385d52b3604190d542dd21e2f2e1aca32cd01c4fd486"

# Date the pins above were verified. Fixed (not "today") so that reruns are idempotent.
PINNED_DATE="2026-10-09"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="${ROOT}/assets/vendor"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/presenter-vendor.XXXXXX")"
trap 'rm -rf "${WORK}"' EXIT
# Everything is assembled here first and moved to ${DEST} at the very end.
OUT="${WORK}/out"

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

# fetch NAME URL SHA256: download, verify and extract into $WORK/NAME/package/.
fetch() {
    local name="$1" url="$2" expected="$3"
    local tarball="${WORK}/${name}.tgz"
    echo "downloading ${url}" >&2
    curl --fail --silent --show-error --location --output "${tarball}" "${url}"
    local actual
    actual="$(sha256_of "${tarball}")"
    if [[ "${actual}" != "${expected}" ]]; then
        echo "error: SHA-256 mismatch for ${url}" >&2
        echo "  expected ${expected}" >&2
        echo "  actual   ${actual}" >&2
        exit 1
    fi
    mkdir -p "${WORK}/${name}"
    tar -xzf "${tarball}" -C "${WORK}/${name}"
}

# copy SRC_ROOT DEST_ROOT REL...: copy each relative path, failing if one is missing.
copy() {
    local src_root="$1" dest_root="$2"
    shift 2
    local rel
    for rel in "$@"; do
        if [[ ! -f "${src_root}/${rel}" ]]; then
            echo "error: expected file missing from package: ${rel}" >&2
            exit 1
        fi
        mkdir -p "$(dirname "${dest_root}/${rel}")"
        cp "${src_root}/${rel}" "${dest_root}/${rel}"
        chmod 0644 "${dest_root}/${rel}"
    done
}

# copy_found SRC_ROOT DEST_ROOT FIND-ARGS...: run `find FIND-ARGS -type f` inside SRC_ROOT and
# copy every file it prints (paths stay relative). Fails if nothing matches.
copy_found() {
    local src_root="$1" dest_root="$2"
    shift 2
    local found rel
    found="$(cd "${src_root}" && find "$@" -type f | LC_ALL=C sort)"
    if [[ -z "${found}" ]]; then
        echo "error: no files match: find $*" >&2
        exit 1
    fi
    while IFS= read -r rel; do
        copy "${src_root}" "${dest_root}" "${rel}"
    done <<<"${found}"
}

vendor_reveal() {
    local src="${WORK}/reveal/package" dest="${OUT}/reveal"
    copy "${src}" "${dest}" \
        LICENSE \
        dist/reveal.js dist/reveal.css dist/reset.css \
        dist/plugin/highlight.js \
        dist/plugin/highlight/monokai.css dist/plugin/highlight/zenburn.css \
        dist/plugin/notes.js dist/plugin/zoom.js dist/plugin/search.js \
        css/theme/template/settings.scss css/theme/template/theme.scss css/theme/template/mixins.scss
    # Stock themes (fonts already inlined as data: URIs) and their SCSS sources.
    copy_found "${src}" "${dest}" dist/theme -maxdepth 1 -name '*.css'
    copy_found "${src}" "${dest}" css/theme -maxdepth 1 -name '*.scss'
    # Theme fonts: the declaring .css, the licenses, and the web-font files the .css
    # references. reveal 6.0.2 ships these fonts as .woff only (no .woff2), so .woff is
    # kept; .ttf/.eot/.otf/.svg are dropped.
    copy_found "${src}" "${dest}" css/theme/fonts \
        \( -name 'LICENSE*' -o -name '*.css' -o -name '*.woff2' -o -name '*.woff' \)
}

# reveal.js ships League Gothic with a LICENSE that only names the SIL Open Font License,
# which asks every copy of the font to carry its copyright notice and the license text.
# LICENSE-OFL.txt adds both: the copyright notice recorded in league-gothic.woff (its
# `name` table, ID 0) and the OFL 1.1 text of Source Sans Pro's LICENSE in the same package.
write_league_gothic_license() {
    local fonts="${OUT}/reveal/css/theme/fonts"
    local out="${fonts}/league-gothic/LICENSE-OFL.txt"
    {
        cat <<'EOF'
League Gothic, the font in league-gothic.woff next to this file, as shipped with reveal.js.

reveal.js's own LICENSE next to this file only names the license. This file, written by
presenter's scripts/vendor.sh, adds what the SIL Open Font License asks every copy of the
font to carry: its copyright notice, as recorded in league-gothic.woff, and the license
text, copied from the Source Sans Pro license that reveal.js ships
(../source-sans-pro/LICENSE).

Copyright (c) 2009 - 2011, The League of Moveable Type

This Font Software is licensed under the SIL Open Font License, Version 1.1.

EOF
        # awk ends every line with a newline, also the last one (the source has none).
        awk 'found || /^SIL OPEN FONT LICENSE Version 1\.1/ { found = 1; print }' \
            "${fonts}/source-sans-pro/LICENSE"
    } >"${out}"
    if ! grep -q '^DISCLAIMER' "${out}"; then
        echo "error: no OFL 1.1 text found in source-sans-pro/LICENSE" >&2
        exit 1
    fi
    chmod 0644 "${out}"
}

vendor_katex() {
    local src="${WORK}/katex/package" dest="${OUT}/katex"
    copy "${src}" "${dest}" LICENSE
    copy "${src}/dist" "${dest}" katex.min.js katex.min.css
    copy_found "${src}/dist" "${dest}" fonts -maxdepth 1 -name '*.woff2'
}

# The license files of the libraries reveal.js bundles (see the pins at the top).
vendor_bundled() {
    copy "${WORK}/highlight.js/package" "${OUT}/highlight.js" LICENSE
    copy "${WORK}/highlightjs-line-numbers.js/package" "${OUT}/highlightjs-line-numbers.js" LICENSE
    copy "${WORK}/marked/package" "${OUT}/marked" LICENSE.md
    copy "${WORK}/fitty/package" "${OUT}/fitty" LICENSE
}

write_versions() {
    cat >"${OUT}/VERSIONS.toml" <<EOF
# Generated by scripts/vendor.sh; do not edit by hand.
# See docs/spec/vendored-assets.md for what is copied and why.

[[package]]
name = "reveal.js"
version = "${REVEAL_VERSION}"
license = "MIT"
url = "${REVEAL_URL}"
sha256 = "${REVEAL_SHA256}"
date = "${PINNED_DATE}"
path = "reveal"

[[package]]
name = "katex"
version = "${KATEX_VERSION}"
license = "MIT"
url = "${KATEX_URL}"
sha256 = "${KATEX_SHA256}"
date = "${PINNED_DATE}"
path = "katex"

# Bundled inside reveal.js's dist/plugin/highlight.js (and the dist/plugin/highlight/*.css
# styles); only its license file is vendored, at the path below.
[[package]]
name = "highlight.js"
version = "${HLJS_VERSION}"
license = "BSD-3-Clause"
url = "${HLJS_URL}"
sha256 = "${HLJS_SHA256}"
date = "${PINNED_DATE}"
path = "highlight.js"
bundled_in = "reveal/dist/plugin/highlight.js"

# Bundled inside reveal.js's dist/plugin/highlight.js (reveal copies it into the plugin's
# source); only its license file is vendored.
[[package]]
name = "highlightjs-line-numbers.js"
version = "${HLJS_LN_VERSION}"
license = "MIT"
url = "${HLJS_LN_URL}"
sha256 = "${HLJS_LN_SHA256}"
date = "${PINNED_DATE}"
path = "highlightjs-line-numbers.js"
bundled_in = "reveal/dist/plugin/highlight.js"

# Bundled inside reveal.js's dist/plugin/notes.js; only its license file is vendored.
[[package]]
name = "marked"
version = "${MARKED_VERSION}"
license = "MIT"
url = "${MARKED_URL}"
sha256 = "${MARKED_SHA256}"
date = "${PINNED_DATE}"
path = "marked"
bundled_in = "reveal/dist/plugin/notes.js"

# Bundled inside reveal.js's dist/reveal.js (it fits text to its box, for r-fit-text);
# only its license file is vendored.
[[package]]
name = "fitty"
version = "${FITTY_VERSION}"
license = "MIT"
url = "${FITTY_URL}"
sha256 = "${FITTY_SHA256}"
date = "${PINNED_DATE}"
path = "fitty"
bundled_in = "reveal/dist/reveal.js"
EOF
    chmod 0644 "${OUT}/VERSIONS.toml"
}

main() {
    fetch reveal "${REVEAL_URL}" "${REVEAL_SHA256}"
    fetch katex "${KATEX_URL}" "${KATEX_SHA256}"
    fetch highlight.js "${HLJS_URL}" "${HLJS_SHA256}"
    fetch highlightjs-line-numbers.js "${HLJS_LN_URL}" "${HLJS_LN_SHA256}"
    fetch marked "${MARKED_URL}" "${MARKED_SHA256}"
    fetch fitty "${FITTY_URL}" "${FITTY_SHA256}"
    mkdir -p "${OUT}"
    vendor_reveal
    write_league_gothic_license
    vendor_katex
    vendor_bundled
    write_versions
    # Swap in the complete tree only now; any failure above exited with DEST unchanged.
    rm -rf "${DEST}"
    mkdir -p "$(dirname "${DEST}")"
    mv "${OUT}" "${DEST}"
    echo "vendored reveal.js ${REVEAL_VERSION} and KaTeX ${KATEX_VERSION}, and the licenses of" \
        "highlight.js ${HLJS_VERSION}, highlightjs-line-numbers.js ${HLJS_LN_VERSION}," \
        "marked ${MARKED_VERSION} and fitty ${FITTY_VERSION}, into ${DEST}" >&2
}

main "$@"
