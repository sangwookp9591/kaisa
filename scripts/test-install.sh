#!/bin/sh
# Offline contract tests. Optional argument selects an installer for mutation checks.
set -eu
installer=${1:-$(dirname "$0")/install.sh}
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT HUP INT TERM
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
release_version=0.0.0-fixture
release="$fixture/releases/download/v$release_version"
mkdir -p "$release" "$fixture/releases/latest/download" "$fixture/tools" "$fixture/home"
printf '%s\n' "$release_version" > "$fixture/releases/latest/download/VERSION"
cat > "$fixture/tools/uname" <<'UNAME'
#!/bin/sh
case "$1" in -s) printf '%s\n' "$TEST_OS" ;; -m) printf '%s\n' "$TEST_ARCH" ;; -r) printf 'fixture-kernel\n' ;; esac
UNAME
chmod +x "$fixture/tools/uname"
checksum() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi
}
a_release_for_target() {
    target=$1
    asset="kaisa-$release_version-$target.tar.gz"
    printf '%s\n' "$target" > "$fixture/kaisa"
    tar -czf "$release/$asset" -C "$fixture" kaisa
    digest=$(checksum "$release/$asset")
    printf '%s  %s\n' "${digest%% *}" "$asset" >> "$release/SHA256SUMS"
}
for target in x86_64-apple-darwin aarch64-apple-darwin x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do
    a_release_for_target "$target"
done
run_installer() {
    HOME="$fixture/home" PATH="$fixture/tools:$PATH" TEST_OS="$os" TEST_ARCH="$arch" \
        KAISA_INSTALL_BASE_URL="file://$fixture/releases" KAISA_INSTALL_DIR="$destination" \
        KAISA_VERSION="$version" sh "$installer"
}
a_supported_platform_installs_its_matching_asset_and_prints_a_path_hint() {
    os=$1 arch=$2 target=$3 version=$4
    destination="$fixture/install $target's bin"
    output=$(run_installer)
    [ "$(cat "$destination/kaisa")" = "$target" ] || fail 'detected OS/arch must choose the matching release asset'
    [ -x "$destination/kaisa" ] || fail 'the created install directory must contain an executable'
    printf '%s\n' "$output" | grep -F 'export PATH=' >/dev/null || fail 'a directory absent from PATH must have a shell hint'
    hint=$(printf '%s\n' "$output" | grep '^export PATH=')
    hinted_path=$(PATH=/usr/bin:/bin sh -c "$hint; printf '%s' \"\$PATH\"")
    [ "$hinted_path" = "$destination:/usr/bin:/bin" ] || fail 'the exact PATH hint must preserve spaces and apostrophes'
    printf '%s\n' "$output" | grep -F 'Open a new terminal, then run: kaisa' >/dev/null || fail 'successful installation must explain the next step'
    printf 'PASS: %s chooses %s (%s)\n' "$os/$arch" "$target" "${version:-latest}"
}
a_supported_platform_installs_its_matching_asset_and_prints_a_path_hint Darwin x86_64 x86_64-apple-darwin "v$release_version"
a_supported_platform_installs_its_matching_asset_and_prints_a_path_hint Darwin arm64 aarch64-apple-darwin ''
a_supported_platform_installs_its_matching_asset_and_prints_a_path_hint Linux x86_64 x86_64-unknown-linux-musl "$release_version"
a_supported_platform_installs_its_matching_asset_and_prints_a_path_hint Linux aarch64 aarch64-unknown-linux-musl ''
an_unset_install_directory_uses_the_isolated_home() {
    destination=''
    run_installer > "$fixture/default.log"
    [ -x "$fixture/home/.local/bin/kaisa" ] || fail 'the default destination must be inside the supplied HOME'
    printf 'PASS: an_unset_install_directory_uses_the_isolated_home\n'
}
an_unset_install_directory_uses_the_isolated_home
a_directory_already_on_path_needs_no_path_hint() {
    destination="$fixture/already-on-path"
    output=$(PATH="$destination:$PATH" run_installer)
    if printf '%s\n' "$output" | grep -F 'export PATH=' >/dev/null; then
        fail 'a directory already on PATH must not need a PATH hint'
    fi
    printf 'PASS: a_directory_already_on_path_needs_no_path_hint\n'
}
a_directory_already_on_path_needs_no_path_hint
a_checksum_mismatch_fails_and_installs_nothing() {
    destination="$fixture/rejected"
    printf 'tampered archive\n' >> "$release/$asset"
    if run_installer > "$fixture/rejected.log" 2>&1; then fail 'a checksum mismatch must fail closed'; fi
    grep -F 'checksum mismatch' "$fixture/rejected.log" >/dev/null || fail 'checksum rejection must explain the mismatch'
    [ ! -e "$destination" ] || fail 'an unverified download must not create the install directory'
    printf 'PASS: a_checksum_mismatch_fails_and_installs_nothing\n'
}
a_checksum_mismatch_fails_and_installs_nothing
a_checksum_mismatch_preserves_an_existing_installation() {
    destination="$fixture/preserved"
    mkdir -p "$destination"
    prior_binary='previous verified binary'
    printf '%s\n' "$prior_binary" > "$destination/kaisa"
    if run_installer > "$fixture/preserved.log" 2>&1; then fail 'a corrupt update must fail'; fi
    [ "$(cat "$destination/kaisa")" = "$prior_binary" ] || fail 'checksum failure must preserve the previous binary'
    printf 'PASS: a_checksum_mismatch_preserves_an_existing_installation\n'
}
a_checksum_mismatch_preserves_an_existing_installation
a_missing_checksum_entry_fails_before_installation() {
    destination="$fixture/no-checksum"
    : > "$release/SHA256SUMS"
    if run_installer > "$fixture/no-checksum.log" 2>&1; then fail 'missing checksums must fail closed'; fi
    grep -F 'checksum entry missing' "$fixture/no-checksum.log" >/dev/null || fail 'a missing checksum must be explained'
    [ ! -e "$destination" ] || fail 'a missing checksum must not create the install directory'
    printf 'PASS: a_missing_checksum_entry_fails_before_installation\n'
}
a_missing_checksum_entry_fails_before_installation
