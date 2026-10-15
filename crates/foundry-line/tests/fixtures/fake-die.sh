#!/bin/sh
set -eu
FAKE_DIE_MODE=normal
stage_task=
previous=
for argument in "$@"; do
  if [ "$previous" = --task ]; then stage_task=$argument; fi
  previous=$argument
done
if [ "$FAKE_DIE_MODE" = fail ]; then exit 9; fi
if [ "$FAKE_DIE_MODE" = timeout ]; then
  (sleep 2; printf 'late write\n' > late-write) &
  wait
  exit 0
fi
if [ "$FAKE_DIE_MODE" = missing ]; then exit 0; fi
if [ "$FAKE_DIE_MODE" = malformed ]; then printf 'invalid\n' > die-output.json; exit 0; fi
if [ "$FAKE_DIE_MODE" = review ] || [ "$FAKE_DIE_MODE" = review-forged ]; then
  case "$stage_task" in
    lens:*|verify:*)
      findings=false
      verdict=pass
      if [ "${stage_task%%:*}" = verify ] && [ -f needs-fix ]; then findings=true; verdict=patch; fi
      printf 'review %s\n' "$stage_task"
      printf '{"findings":%s,"verdict_category":"%s","verdict_categories":[],"inspected_commit":"%s"}\n' "$findings" "$verdict" "$(git rev-parse HEAD)" > die-output.json
      exit 0
      ;;
    *) rm -f needs-fix; git add -u ;;
  esac
fi
if [ "$FAKE_DIE_MODE" = schema ]; then printf '{"findings":false}\n' > die-output.json; exit 0; fi
if [ "$FAKE_DIE_MODE" = verify ]; then
  printf 'verified\n'
  printf '{"findings":false,"verdict_category":"pass","verdict_categories":[],"inspected_commit":"%s"}\n' "$(git rev-parse HEAD)" > die-output.json
  exit 0
fi
if [ "$FAKE_DIE_MODE" = manifest ]; then
  sed 's/order-1:implement/..\/escaped/' input-manifest.json > manifest.tmp
  mv manifest.tmp input-manifest.json
fi
git config user.email fake-die@example.invalid
git config user.name fake-die
base_commit=$(git rev-parse HEAD)
if [ "$FAKE_DIE_MODE" = unrelated ] || [ "$FAKE_DIE_MODE" = replace ]; then git checkout --orphan unrelated >/dev/null 2>&1; fi
printf 'die output %s\n' "$*" >> die-change.txt
git add die-change.txt
git commit -m 'fake Die change' >/dev/null
commit=$(git rev-parse HEAD)
if [ "$FAKE_DIE_MODE" = replace ]; then git replace --graft "$commit" "$base_commit"; fi
if [ "$FAKE_DIE_MODE" = forged ] || [ "$FAKE_DIE_MODE" = review-forged ]; then commit=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa; fi
printf '{"produced_commit":"%s","findings":false,"verdict_category":null}\n' "$commit" > die-output.json
if [ "$FAKE_DIE_MODE" = background ]; then
  (sleep 2; printf 'late write\n' > late-write) &
fi
