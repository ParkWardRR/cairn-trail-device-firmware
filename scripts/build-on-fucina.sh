#!/usr/bin/env bash
# Run scripts/ci.sh on fucina, in a rootless podman container, and bring the
# artifacts back into dist/.
#
#   scripts/build-on-fucina.sh              # the whole of ci.sh, remotely
#   scripts/build-on-fucina.sh --no-pull    # build there, leave dist/ alone
#
# Why: builds for this project do not run on the Mac. fucina has the cores and
# the RAM, and the Xtensa toolchain lives in an image rather than on the host.
# The container is built from scripts/fucina/Containerfile and pins the same
# esp toolchain version the Mac uses, so a binary built either way is the same
# binary. Flashing still happens from the Mac -- it is not a build.
set -euo pipefail
cd "$(dirname "$0")/.."

HOST=${FUCINA_HOST:-alfa@fucina.alpina.casa}
IMAGE=${FUCINA_IMAGE:-localhost/cairn-trail-rust:1}
REMOTE=${FUCINA_DIR:-builds/cairn-trail}
PULL=1
[ "${1:-}" = "--no-pull" ] && PULL=0

step() { printf '\n== %s\n' "$*"; }

# fucina is mandatory when it has capacity, and the check must be cheap: the
# AV1 worker that shares this host freezes the moment a build starts, so a raw
# load average overstates how busy it is. `build-capacity` reports without it.
step "capacity"
cap=$(ssh -o ConnectTimeout=5 "$HOST" build-capacity) || {
  echo "fucina did not answer. Build elsewhere, and say so in the report." >&2
  exit 1
}
echo "$cap"
load=${cap#load=}
load=${load%% *}
mem=${cap##*memGB=}
awk -v l="${load:-99}" -v m="${mem:-0}" 'BEGIN { exit !(l < 5 && m >= 4) }' || {
  echo "fucina is busy ($cap). Build elsewhere, and say so in the report." >&2
  exit 1
}

step "toolchain image"
ssh "$HOST" "podman image exists $IMAGE" || {
  ssh "$HOST" "mkdir -p ~/$REMOTE-img"
  scp -q scripts/fucina/Containerfile "$HOST:$REMOTE-img/Containerfile"
  ssh "$HOST" "cd ~/$REMOTE-img && podman build -t $IMAGE ."
}

# The tree, minus everything that is an output or a local toolchain's work.
# .git goes too: ci.sh stamps the build with the commit and whether it is dirty.
step "sync"
ssh "$HOST" "mkdir -p ~/$REMOTE \
  ~/builds/.cache/cairn-trail/{registry,git,target,home-cache}"
rsync -a --delete \
  --exclude 'target/' --exclude 'dist/' --exclude 'pulled/' --exclude '.pio/' \
  ./ "$HOST:$REMOTE/"

# Caches live outside the synced tree so --delete cannot throw them away, and
# are bind-mounted where cargo expects them. CARGO_TARGET_DIR covers every
# crate in the repo, which is what makes a second run quick.
step "ci.sh in $IMAGE"
ssh -t "$HOST" "podman run --rm --userns=keep-id \
  -v \"\$HOME/$REMOTE:/work:z\" \
  -v \"\$HOME/builds/.cache/cairn-trail/registry:/home/esp/.cargo/registry:z\" \
  -v \"\$HOME/builds/.cache/cairn-trail/git:/home/esp/.cargo/git:z\" \
  -v \"\$HOME/builds/.cache/cairn-trail/target:/work-target:z\" \
  -v \"\$HOME/builds/.cache/cairn-trail/home-cache:/home/esp/.cache:z\" \
  -e CARGO_TARGET_DIR=/work-target \
  -e CARGO_NET_GIT_FETCH_WITH_CLI=true \
  -w /work $IMAGE bash -lc 'scripts/ci.sh'"

if [ "$PULL" = 1 ]; then
  step "artifacts"
  rm -rf dist && mkdir -p dist
  rsync -a "$HOST:$REMOTE/dist/" dist/
  (cd dist && shasum -a 256 -c SHA256SUMS)
  ls -l dist
fi
