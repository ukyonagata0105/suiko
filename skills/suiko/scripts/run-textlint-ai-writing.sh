#!/bin/sh
set -eu

if [ "$#" -eq 0 ]; then
    echo "usage: run-textlint-ai-writing.sh <file> [file ...]" >&2
    exit 2
fi

if ! command -v node >/dev/null 2>&1 || ! command -v npm >/dev/null 2>&1; then
    echo "textlint check skipped: Node.js and npm are required" >&2
    exit 3
fi

if ! node -e 'const [major, minor] = process.versions.node.split(".").map(Number); process.exit(major > 20 || (major === 20 && minor >= 18) ? 0 : 1)'; then
    echo "textlint check skipped: Node.js 20.18 or later is required" >&2
    exit 3
fi

exec npm exec --yes \
    --package=textlint@15.8.0 \
    --package=@textlint-ja/textlint-rule-preset-ai-writing@1.7.0 \
    -- textlint \
    --no-textlintrc \
    --preset @textlint-ja/ai-writing \
    --format json \
    -- "$@"
