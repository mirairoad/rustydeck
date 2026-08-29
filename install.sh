#!/usr/bin/env bash
#
# Build RustyDeck from source and install it.
#
#   curl -fsSL https://raw.githubusercontent.com/mirairoad/rustydeck/main/install.sh | bash
#
# Or, better, read it first:
#
#   curl -fsSLO https://raw.githubusercontent.com/mirairoad/rustydeck/main/install.sh
#   less install.sh && bash install.sh
#
# Building on the machine it will run on is deliberate rather than a shortcut. A prebuilt binary
# links against a particular ICU soname - libicuuc.so.78 on Arch today, .74 on Ubuntu 24.04 - and
# would refuse to start on the other. Compiling here links against whatever is actually installed.
#
# Options:
#   --ref <tag|branch>   what to build (default: main)
#   --prefix <dir>       where the binary goes (default: /usr/local)
#   --skip-deps          do not check for build dependencies
#   --uninstall          remove what a previous run installed
#   --help

set -euo pipefail

REPO="https://github.com/mirairoad/rustydeck.git"
REF="main"
PREFIX="/usr/local"
SKIP_DEPS=0
UNINSTALL=0

BOLD=$'\033[1m'; DIM=$'\033[2m'; RED=$'\033[31m'; GREEN=$'\033[32m'; YELLOW=$'\033[33m'; OFF=$'\033[0m'
say()  { printf '%s==>%s %s\n' "$BOLD" "$OFF" "$*"; }
warn() { printf '%s==>%s %s\n' "$YELLOW" "$OFF" "$*" >&2; }
die()  { printf '%s==>%s %s\n' "$RED" "$OFF" "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
	case "$1" in
		--ref)       REF="${2:?--ref needs a tag or branch}"; shift 2 ;;
		--prefix)    PREFIX="${2:?--prefix needs a directory}"; shift 2 ;;
		--skip-deps) SKIP_DEPS=1; shift ;;
		--uninstall) UNINSTALL=1; shift ;;
		--help|-h)   sed -n '3,22p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
		*)           die "Unknown option: $1" ;;
	esac
done

# Sudo only where it is needed, and not at all when already root. Asking for the whole script to be
# run as root would mean building as root too, which is both unnecessary and a good way to end up
# with a cargo cache nobody can write to.
SUDO=""
if [ "$(id -u)" -ne 0 ]; then
	command -v sudo >/dev/null || die "This needs sudo to install to $PREFIX, and sudo is not installed."
	SUDO="sudo"
fi

BIN="$PREFIX/bin/rustydeck"
UDEV="/usr/lib/udev/rules.d/40-streamdeck.rules"
DESKTOP="/usr/share/applications/rustydeck.desktop"
ICON="/usr/share/icons/hicolor/512x512/apps/rustydeck.png"

if [ "$UNINSTALL" -eq 1 ]; then
	say "Removing RustyDeck"
	$SUDO rm -fv "$BIN" "$UDEV" "$DESKTOP" "$ICON"
	$SUDO udevadm control --reload-rules 2>/dev/null || true
	say "Done. Your configuration in ~/.rustydeck was left alone."
	exit 0
fi

# ---------------------------------------------------------------------------
# Build dependencies
# ---------------------------------------------------------------------------

# Checked by pkg-config name rather than package name, because the package names differ per distro
# and the pkg-config names do not.
REQUIRED_PC="libudev wayland-client xkbcommon fontconfig freetype2 vulkan gtk+-3.0 ayatana-appindicator3-0.1 dbus-1"

packages_for() {
	case "$1" in
		arch)   echo "rust git base-devel gtk3 systemd-libs vulkan-icd-loader libxkbcommon fontconfig libayatana-appindicator wayland" ;;
		debian) echo "cargo git build-essential pkg-config libgtk-3-dev libudev-dev libvulkan-dev libxkbcommon-dev libfontconfig-dev libfreetype-dev libayatana-appindicator3-dev libwayland-dev libdbus-1-dev" ;;
		fedora) echo "cargo git gcc pkgconf gtk3-devel systemd-devel vulkan-loader-devel libxkbcommon-devel fontconfig-devel freetype-devel libayatana-appindicator-gtk3-devel wayland-devel dbus-devel" ;;
		*)      echo "" ;;
	esac
}

install_command_for() {
	case "$1" in
		arch)   echo "sudo pacman -S --needed $(packages_for arch)" ;;
		debian) echo "sudo apt install -y $(packages_for debian)" ;;
		fedora) echo "sudo dnf install -y $(packages_for fedora)" ;;
		*)      echo "" ;;
	esac
}

detect_distro() {
	[ -r /etc/os-release ] || { echo unknown; return; }
	# shellcheck disable=SC1091
	. /etc/os-release
	case " ${ID:-} ${ID_LIKE:-} " in
		*" arch "*|*" archlinux "*)   echo arch ;;
		*" debian "*|*" ubuntu "*)    echo debian ;;
		*" fedora "*|*" rhel "*)      echo fedora ;;
		*)                            echo unknown ;;
	esac
}

check_dependencies() {
	local distro missing=""
	distro="$(detect_distro)"

	command -v cargo >/dev/null || missing="$missing cargo"
	command -v git >/dev/null || missing="$missing git"
	command -v cc >/dev/null || missing="$missing a-C-compiler"

	if command -v pkg-config >/dev/null; then
		local pc
		for pc in $REQUIRED_PC; do
			pkg-config --exists "$pc" 2>/dev/null || missing="$missing $pc"
		done
	else
		missing="$missing pkg-config"
	fi

	[ -z "$missing" ] && { say "Build dependencies: ${GREEN}all present${OFF}"; return; }

	warn "Missing build dependencies:$missing"
	local command
	command="$(install_command_for "$distro")"
	if [ -n "$command" ]; then
		printf '\n  %s\n\n' "$command"
		# Not run automatically: installing system packages is the user's decision, and a script
		# piped from the internet is the last thing that should be making it silently.
		die "Install those and run this again, or pass --skip-deps if you know better."
	fi
	die "Install the equivalents for your distribution, or pass --skip-deps."
}

[ "$SKIP_DEPS" -eq 1 ] || check_dependencies

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

say "Fetching $REF"
git clone --quiet --depth 1 --branch "$REF" "$REPO" "$WORK/rustydeck" \
	|| die "Could not fetch $REF from $REPO"

say "Building - this takes a few minutes, and is quiet while it works"
( cd "$WORK/rustydeck/src" && cargo build --release --quiet ) || die "Build failed"

BUILT="$WORK/rustydeck/src/target/release/rustydeck"
[ -x "$BUILT" ] || die "Build reported success but produced no binary"

# ---------------------------------------------------------------------------
# Install
# ---------------------------------------------------------------------------

say "Installing to $PREFIX (this is the part that needs sudo)"
$SUDO install -Dm755 "$BUILT" "$BIN"
$SUDO install -Dm644 "$WORK/rustydeck/src/bundle/40-streamdeck.rules" "$UDEV"
$SUDO install -Dm644 "$WORK/rustydeck/src/bundle/rustydeck.desktop" "$DESKTOP"
$SUDO install -Dm644 "$WORK/rustydeck/src/icons/icon.png" "$ICON"

# What lets the app open the deck without root. Without this you get a device that enumerates and
# then refuses to talk, which looks like a broken app rather than a permissions problem.
say "Reloading udev rules"
$SUDO udevadm control --reload-rules
$SUDO udevadm trigger --subsystem-match=usb --attr-match=idVendor=0fd9 2>/dev/null || true

printf '\n%sRustyDeck is installed.%s\n' "$GREEN$BOLD" "$OFF"
printf '  %sRun it with:%s      rustydeck\n' "$DIM" "$OFF"
printf '  %sIf your deck is%s   already plugged in, unplug and replug it once so the new\n' "$DIM" "$OFF"
printf '  %spermissions apply.%s\n' "$DIM" "$OFF"
printf '  %sUninstall with:%s   bash install.sh --uninstall\n\n' "$DIM" "$OFF"
