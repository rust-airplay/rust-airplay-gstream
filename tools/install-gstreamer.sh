#!/usr/bin/env bash
# Install the GStreamer packages this repository's CI needs on Ubuntu.
#
# The Ubuntu runner image is not a clean distribution, and one difference matters here:
# it ships LLVM's libunwind-<version>-dev packages, and GNU libunwind-dev -- which
# libgstreamer1.0-dev depends on -- conflicts with them over the same files. apt will not
# remove installed packages to satisfy a dependency, so it reports the dependency as
# unsatisfiable, as `libgstreamer1.0-dev : Depends: libunwind-dev` followed by
# `E: Unable to correct problems, you have held broken packages`. The removal has to be
# explicit; on a CI runner the cost is clang and lldb, which nothing here uses.
#
# The rest is defence for the same symptom from other causes: a component the image
# happens not to have enabled, a package the image holds, a half-finished upgrade. Each
# is cheap and each turns a red job into a line in the log, and a genuine failure still
# fails -- once, with the versions apt resolved printed.
set -euo pipefail

mode="${1:-dev}"
case "$mode" in
  dev)
    packages=(libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev)
    ;;
  runtime)
    # h264parse and h265parse are in plugins-bad, avdec_* in gst-libav, videoconvert and
    # audioconvert in plugins-base, wavparse in plugins-good, and the two inspection
    # tools in gstreamer1.0-tools. plugins-ugly is deliberately absent: nothing here
    # needs it, and it lives in multiverse.
    packages=(
      libgstreamer1.0-dev
      libgstreamer-plugins-base1.0-dev
      gstreamer1.0-plugins-base
      gstreamer1.0-plugins-good
      gstreamer1.0-plugins-bad
      gstreamer1.0-libav
      gstreamer1.0-tools
    )
    ;;
  *)
    echo "usage: $0 [dev|runtime]" >&2
    exit 2
    ;;
esac

if command -v add-apt-repository >/dev/null 2>&1; then
  sudo add-apt-repository -y universe >/dev/null
fi
sudo apt-get update

# LLVM's libunwind development packages are what GNOME's, and therefore GStreamer's,
# dependency cannot coexist with. Removing them is what lets apt resolve the install.
conflicting="$(
  dpkg-query -W -f='${Package}\n' 'libunwind-*-dev' 2>/dev/null |
    grep -vx 'libunwind-dev' || true
)"
if [ -n "$conflicting" ]; then
  echo "removing the LLVM libunwind development packages that conflict with libunwind-dev: $conflicting"
  # Word splitting is the point: one name per line, one argument each.
  # shellcheck disable=SC2086
  sudo apt-get remove -y $conflicting
fi

held="$(apt-mark showhold || true)"
if [ -n "$held" ]; then
  # shellcheck disable=SC2086
  sudo apt-mark unhold $held
fi

sudo apt-get --fix-broken install -y || true

install_packages() {
  sudo apt-get install -y --no-install-recommends "$@"
}

if install_packages "${packages[@]}"; then
  exit 0
fi

echo "::warning::the GStreamer install failed; recording what apt resolved before retrying"
apt-cache policy libunwind-dev libunwind8 gstreamer1.0-tools || true
sudo apt-get update
sudo apt-get --fix-broken install -y || true
install_packages "${packages[@]}"
