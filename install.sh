#!/bin/sh
# Install Rook from a published release.
#
#   curl -fsSL https://raw.githubusercontent.com/ASlava12/rook/main/install.sh | sh
#
# What it does, in the order it does it: works out which build this machine
# wants, downloads that archive and the release's checksums, refuses to go on if
# they disagree, and copies two binaries and the built-in skills into a
# directory under your home. It writes nowhere else, asks for no privileges, and
# does not touch the shell's configuration — the last thing it does is tell you
# whether the directory it used is on your PATH.
#
# `sh`, not `bash`: a script piped into a shell should run under the one that is
# there. Nothing here needs more.

set -eu

REPO="${ROOK_REPO:-ASlava12/rook}"
# Where a user-owned install belongs, and the layout the binaries themselves
# look for: `bin/rook` finds its skills at `../share/rook/skills`.
PREFIX="${ROOK_PREFIX:-$HOME/.local}"
VERSION="${ROOK_VERSION:-latest}"

say() { printf '%s\n' "$*"; }
die() { printf 'install: %s\n' "$*" >&2; exit 1; }

need() {
    command -v "$1" > /dev/null 2>&1 || die "$1 is needed and is not on PATH"
}

# One of the triples the release workflow builds. An unknown pair is said out
# loud with what to do instead, rather than downloading something that will not
# run.
target_for() {
    os="$(uname -s)"
    arch="$(uname -m)"
    case "$os" in
        Linux) os_part="unknown-linux-musl" ;;
        Darwin) os_part="apple-darwin" ;;
        MINGW* | MSYS* | CYGWIN*)
            die "on Windows use PowerShell: irm https://raw.githubusercontent.com/$REPO/main/install.ps1 | iex"
            ;;
        FreeBSD)
            die "FreeBSD is a supported target with no published build yet — \`cargo install --path crates/rook-cli\` from a clone"
            ;;
        *) die "no published build for $os; build from source with cargo" ;;
    esac
    case "$arch" in
        x86_64 | amd64) arch_part="x86_64" ;;
        arm64 | aarch64) arch_part="aarch64" ;;
        *) die "no published build for $arch; build from source with cargo" ;;
    esac
    printf '%s-%s' "$arch_part" "$os_part"
}

fetch() {
    # `-f` so a 404 is a failure rather than a file containing the word "Not
    # Found", which is the shape of every install script that ever wrote HTML
    # to a binary.
    if command -v curl > /dev/null 2>&1; then
        curl -fsSL "$1" -o "$2"
    else
        wget -q "$1" -O "$2"
    fi
}

command -v curl > /dev/null 2>&1 || command -v wget > /dev/null 2>&1 ||
    die "curl or wget is needed"
need tar
need uname

TARGET="$(target_for)"
case "$VERSION" in
    latest) BASE="https://github.com/$REPO/releases/latest/download" ;;
    *) BASE="https://github.com/$REPO/releases/download/$VERSION" ;;
esac

TMP="$(mktemp -d)"
# Whatever happens next, including a checksum that does not match.
trap 'rm -rf "$TMP"' EXIT INT TERM

say "rook: fetching $TARGET from $REPO ($VERSION)"
# The archive name carries the version, which `latest` does not know yet — so
# the checksums are fetched first and the name is read out of them.
fetch "$BASE/SHA256SUMS" "$TMP/SHA256SUMS" ||
    die "no published release to install from yet — build from a clone with \`cargo xtask dist\`"
ARCHIVE="$(awk -v t="$TARGET" '$2 ~ t {print $2}' "$TMP/SHA256SUMS" | head -n 1)"
[ -n "$ARCHIVE" ] || die "the release has no build for $TARGET"

fetch "$BASE/$ARCHIVE" "$TMP/$ARCHIVE" || die "could not download $ARCHIVE"

# Verified before anything is unpacked. A download nobody checked is a download
# somebody else can replace.
expected="$(awk -v a="$ARCHIVE" '$2 == a {print $1}' "$TMP/SHA256SUMS" | head -n 1)"
if command -v sha256sum > /dev/null 2>&1; then
    got="$(sha256sum "$TMP/$ARCHIVE" | awk '{print $1}')"
elif command -v shasum > /dev/null 2>&1; then
    got="$(shasum -a 256 "$TMP/$ARCHIVE" | awk '{print $1}')"
else
    die "neither sha256sum nor shasum is here, and an unverified download is not installed"
fi
[ "$expected" = "$got" ] ||
    die "the checksum does not match: expected $expected, got $got — nothing was installed"
say "rook: checksum ok"

tar xzf "$TMP/$ARCHIVE" -C "$TMP"
UNPACKED="$TMP/${ARCHIVE%.tar.gz}"
[ -d "$UNPACKED/bin" ] || die "the archive is not shaped as expected: no bin/ in $ARCHIVE"

mkdir -p "$PREFIX/bin" "$PREFIX/share/rook"
# Replaced rather than written over: a binary being executed cannot be
# overwritten on some systems, and a half-written one is worse than an old one.
for binary in rook rookd; do
    cp "$UNPACKED/bin/$binary" "$PREFIX/bin/$binary.new"
    chmod +x "$PREFIX/bin/$binary.new"
    mv "$PREFIX/bin/$binary.new" "$PREFIX/bin/$binary"
done
rm -rf "$PREFIX/share/rook/skills"
cp -R "$UNPACKED/share/rook/skills" "$PREFIX/share/rook/skills"

say "rook: installed to $PREFIX/bin"
"$PREFIX/bin/rook" --version || true

case ":$PATH:" in
    *":$PREFIX/bin:"*) ;;
    *)
        say ""
        say "$PREFIX/bin is not on your PATH. Add it:"
        say "  echo 'export PATH=\"$PREFIX/bin:\$PATH\"' >> ~/.profile"
        ;;
esac

say ""
say "Next: rook init, then rook — or read $PREFIX/share/rook/skills for what it ships with."
