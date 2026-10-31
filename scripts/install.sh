#!/bin/sh
# Install a checksum-verified release. The override base uses GitHub's releases layout.
set -eu
main() {
fail() { printf 'kaisa install: %s\n' "$*" >&2; exit 1; }
command -v curl >/dev/null 2>&1 || fail 'curl is required'
case "$(uname -s)" in
    Darwin) platform=apple-darwin ;;
    Linux) platform=unknown-linux-musl
        case "$(uname -r)" in *[Mm]icrosoft*) printf 'WSL detected: installing the Linux binary.\n' ;; esac ;;
    *) fail 'unsupported OS; use install.ps1 on Windows' ;;
esac
case "$(uname -m)" in
    x86_64|amd64) arch=x86_64 ;;
    arm64|aarch64) arch=aarch64 ;;
    *) fail 'unsupported architecture (expected x86_64 or aarch64)' ;;
esac
if [ "$platform" = apple-darwin ] && [ "$arch" = x86_64 ] &&
    [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || :)" = 1 ]; then
    arch=aarch64
fi
base=${KAISA_INSTALL_BASE_URL:-https://github.com/sangwookp9591/kaisa/releases}
base=${base%/}
work=$(mktemp -d) || fail 'cannot create temporary directory'
trap 'rm -rf "$work"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
fetch() {
    case "$base" in
        https://*) curl --proto '=https' --proto-redir '=https' --tlsv1.2 -fsSL "$1" -o "$2" ;;
        *) curl -fsSL "$1" -o "$2" ;;
    esac || fail "download failed: $1"
}
version=${KAISA_VERSION:-}
if [ -z "$version" ]; then
    fetch "$base/latest/download/VERSION" "$work/VERSION"
    version=$(cat "$work/VERSION")
fi
version=${version#v}
case "$version" in ''|*[!0-9A-Za-z.+-]*) fail 'invalid release version' ;; esac
asset="kaisa-$version-$arch-$platform.tar.gz"
url="$base/download/v$version"
fetch "$url/$asset" "$work/$asset"
fetch "$url/SHA256SUMS" "$work/SHA256SUMS"
expected=$(awk -v name="$asset" '$2 == name { print $1; count++ } END { if (count != 1) exit 1 }' "$work/SHA256SUMS") || fail 'checksum entry missing or duplicated'
[ "${#expected}" -eq 64 ] || fail 'invalid SHA256 checksum'
case "$expected" in *[!0-9a-fA-F]*) fail 'invalid SHA256 checksum' ;; esac
if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$work/$asset")
elif command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$work/$asset")
else
    fail 'sha256sum or shasum is required to verify the checksum'
fi
expected=$(printf '%s' "$expected" | tr 'A-F' 'a-f')
actual=${actual%% *}
[ "$actual" = "$expected" ] || fail "checksum mismatch for $asset; nothing installed"
# Extract only the expected binary; never unpack arbitrary archive paths into the destination.
tar -xzf "$work/$asset" -C "$work" kaisa || fail 'archive does not contain kaisa'
[ -f "$work/kaisa" ] && [ ! -L "$work/kaisa" ] || fail 'archive binary must be a regular file'
install_dir=${KAISA_INSTALL_DIR:-$HOME/.local/bin}
case "$install_dir" in /*) ;; *) fail 'KAISA_INSTALL_DIR must be an absolute path' ;; esac
mkdir -p "$install_dir"
# Stage on the destination filesystem so replacing an existing binary is atomic.
staged=$(mktemp "$install_dir/.kaisa.XXXXXX") || fail 'cannot stage binary'
trap 'rm -rf "$work"; rm -f "$staged"' EXIT
cp "$work/kaisa" "$staged"
chmod 755 "$staged"
mv -f "$staged" "$install_dir/kaisa"
printf 'Installed kaisa %s to %s/kaisa\n' "$version" "$install_dir"
case ":${PATH:-}:" in
    *":$install_dir:"*) ;;
    *)
        # Single-quote escaping keeps spaces, dollar signs and apostrophes literal.
        quoted_dir=$(printf '%s' "$install_dir" | sed "s/'/'\\\\''/g")
        # Literal profile names are instructions, not paths opened by this script.
        # shellcheck disable=SC2088
        case "${SHELL:-}" in
            */zsh|zsh) profile='~/.zshrc' ;;
            */bash|bash)
                if [ "$platform" = apple-darwin ]; then profile='~/.bash_profile'; else profile='~/.bashrc'; fi ;;
            */fish|fish) profile='~/.config/fish/config.fish' ;;
            *) profile='~/.profile' ;;
        esac
        printf 'Add this exact line to %s, then open a new terminal:\n' "$profile"
        case "${SHELL:-}" in
            */fish|fish)
                # Fish single quotes escape backslashes and apostrophes with a backslash.
                fish_dir=$(printf '%s' "$install_dir" | sed "s/\\\\/\\\\\\\\/g; s/'/\\\\'/g")
                printf "fish_add_path -- '%s'\n" "$fish_dir" ;;
            *) printf "export PATH='%s':\"\$PATH\"\n" "$quoted_dir" ;;
        esac
        ;;
esac
printf 'Open a new terminal, then run: kaisa\n'
}
main "$@"
