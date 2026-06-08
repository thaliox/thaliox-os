#!/usr/bin/env bash
# Provision a fresh **KVM bare-metal** host to run THALIOX Firecracker microVMs
# (RFC-0004 / M2). Idempotent — safe to re-run. Run ON the host as root.
#
#   ./fc-host-setup.sh
#   WORKDIR=/root/fc ./fc-host-setup.sh                 # custom workdir
#   GH_MIRROR=https://gh.tkshub.cloud ./fc-host-setup.sh # mainland host behind GFW
#
# Pinned versions reproduce the exact environment M2 was validated on.
# The guest runner is NOT built here — it is cross-compiled on the dev host and
# rsync'd over (see the printed "Next" steps).

set -euo pipefail

FC_VERSION="${FC_VERSION:-v1.16.0}"           # firecracker release
CI_VERSION="${CI_VERSION:-v1.15}"             # firecracker CI artifacts (kernel/rootfs)
KERNEL="${KERNEL:-vmlinux-5.10.245}"
ROOTFS="${ROOTFS:-ubuntu-24.04.squashfs}"
ARCH="$(uname -m)"
GH="${GH_MIRROR:-https://github.com}"         # github base (override for the mainland mirror)
WORKDIR="${WORKDIR:-$([ -d /mnt/data ] && echo /mnt/data/firecracker || echo "$HOME/fc")}"

echo "== 1. KVM precheck =="
[ -e /dev/kvm ] || { echo "FATAL: /dev/kvm missing — need bare metal or nested-virt"; exit 1; }
flags=$(grep -Ec '(vmx|svm)' /proc/cpuinfo || true)
[ "${flags:-0}" -gt 0 ] || { echo "FATAL: no vmx/svm virtualization flags"; exit 1; }
echo "  /dev/kvm OK · virt flags: $flags · arch: $ARCH · workdir: $WORKDIR"

echo "== 2. packages =="
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq curl tar squashfs-tools e2fsprogs rsync

mkdir -p "$WORKDIR" && cd "$WORKDIR"

echo "== 3. firecracker $FC_VERSION + jailer =="
if [ ! -x ./firecracker ]; then
  curl -fSL "$GH/firecracker-microvm/firecracker/releases/download/$FC_VERSION/firecracker-$FC_VERSION-$ARCH.tgz" -o fc.tgz
  tar -xzf fc.tgz
  cp "release-$FC_VERSION-$ARCH/firecracker-$FC_VERSION-$ARCH" ./firecracker
  cp "release-$FC_VERSION-$ARCH/jailer-$FC_VERSION-$ARCH" ./jailer
  chmod +x firecracker jailer
fi
./firecracker --version | head -1

echo "== 4. guest kernel + rootfs (Firecracker CI $CI_VERSION — direct S3) =="
S3="https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/$CI_VERSION/$ARCH"
[ -f vmlinux ]         || curl -fSL "$S3/$KERNEL" -o vmlinux
[ -f rootfs.squashfs ] || curl -fSL "$S3/$ROOTFS" -o rootfs.squashfs

echo "== 5. unpack rootfs + build a base writable ext4 (plain boot) =="
rm -rf squashfs-root
unsquashfs -q -d squashfs-root rootfs.squashfs >/dev/null
truncate -s 1G rootfs.ext4
mkfs.ext4 -F -q -d squashfs-root rootfs.ext4
echo "  artifacts:"; ls -lh firecracker vmlinux rootfs.ext4 | awk '{print "   "$5"  "$9}'

cat <<EOF

== provisioned: $WORKDIR ==

Next — deploy the guest runner FROM the dev host (no Rust needed here):
  cargo build --release --target x86_64-unknown-linux-musl -p thaliox-guest-runner
  rsync -az target/x86_64-unknown-linux-musl/release/thaliox-runner root@<HOST>:$WORKDIR/
  ssh root@<HOST> 'cd $WORKDIR; cp thaliox-runner squashfs-root/usr/bin/; \\
    chmod +x squashfs-root/usr/bin/thaliox-runner; \\
    truncate -s 1G rootfs-runner.ext4; mkfs.ext4 -F -q -d squashfs-root rootfs-runner.ext4'

Validate (on the host, in $WORKDIR) — the M2 acceptance checklist:
  [F1] plain boot + snapshot/restore  : manual API smoke (RFC-0004 §10), or just:
  [F3] ./thaliox-runner fc-launch   ./firecracker ./vmlinux ./rootfs-runner.ext4 .
       expect: deploy/health/checkpoint/shutdown over vsock; firecracker exits clean
  [F4] ./thaliox-runner fc-snapshot ./firecracker ./vmlinux ./rootfs-runner.ext4 .
       expect: snapshot -> kill -> restore -> health budget survives (no re-deploy)
EOF
