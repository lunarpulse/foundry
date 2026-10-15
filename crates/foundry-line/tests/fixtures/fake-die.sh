#!/bin/sh
set -eu
FAKE_DIE_MODE=normal
if [ "$FAKE_DIE_MODE" = fail ]; then exit 9; fi
git config user.email fake-die@example.invalid
git config user.name fake-die
printf 'die output\n' > die-change.txt
git add die-change.txt
git commit -m 'fake Die change' >/dev/null
commit=$(git rev-parse HEAD)
if [ "$FAKE_DIE_MODE" = forged ]; then commit=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa; fi
printf '{"produced_commit":"%s","findings":false,"verdict_category":null}\n' "$commit" > die-output.json
