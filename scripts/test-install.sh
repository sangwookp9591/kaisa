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
case "$1" in -s) printf '%s\n' "$TEST_OS" ;; -m) printf '%s\n' "$TEST_ARCH" ;; -r) printf '%s\n' "${TEST_KERNEL:-fixture-kernel}" ;; esac
UNAME
cat > "$fixture/tools/sysctl" <<'SYSCTL'
#!/bin/sh
[ "$1 $2" = '-n sysctl.proc_translated' ] || exit 1
printf '%s\n' "${TEST_TRANSLATED:-0}"
SYSCTL
chmod +x "$fixture/tools/uname" "$fixture/tools/sysctl"
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
    HOME="$fixture/home" PATH="$fixture/tools:$PATH" TEST_OS="$os" TEST_ARCH="$arch" SHELL="${TEST_SHELL:-/bin/sh}" \
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
# Every failure case restores pristine fixtures instead of inheriting another test's corruption.
os=Linux arch=aarch64 version=$release_version
asset="kaisa-$release_version-aarch64-unknown-linux-musl.tar.gz"
cp "$release/SHA256SUMS" "$fixture/pristine-sums"
cp "$release/$asset" "$fixture/pristine-archive"
a_checksum_mismatch_fails_and_installs_nothing() {
    destination="$fixture/rejected"
    printf 'tampered archive\n' >> "$release/$asset"
    if run_installer > "$fixture/rejected.log" 2>&1; then fail 'a checksum mismatch must fail closed'; fi
    grep -F 'checksum mismatch' "$fixture/rejected.log" >/dev/null || fail 'checksum rejection must explain the mismatch'
    [ ! -e "$destination" ] || fail 'an unverified download must not create the install directory'
    cp "$fixture/pristine-archive" "$release/$asset"
    printf 'PASS: a_checksum_mismatch_fails_and_installs_nothing\n'
}
a_checksum_mismatch_fails_and_installs_nothing
a_checksum_mismatch_preserves_an_existing_installation() {
    destination="$fixture/preserved"
    mkdir -p "$destination"
    prior_binary='previous verified binary'
    printf '%s\n' "$prior_binary" > "$destination/kaisa"
    printf 'tampered\n' >> "$release/$asset"
    if run_installer > "$fixture/preserved.log" 2>&1; then fail 'a corrupt update must fail'; fi
    [ "$(cat "$destination/kaisa")" = "$prior_binary" ] || fail 'checksum failure must preserve the previous binary'
    cp "$fixture/pristine-archive" "$release/$asset"
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

cp "$fixture/pristine-sums" "$release/SHA256SUMS"
TEST_TRANSLATED=1 a_supported_platform_installs_its_matching_asset_and_prints_a_path_hint Darwin x86_64 aarch64-apple-darwin "$release_version"
for shell_case in 'Darwin zsh ~/.zshrc' 'Darwin bash ~/.bash_profile' 'Linux bash ~/.bashrc' 'Linux fish ~/.config/fish/config.fish'; do
    # Controlled table values contain no whitespace within fields.
    # shellcheck disable=SC2086
    set -- $shell_case
    os=$1 arch=x86_64 destination="$fixture/shell $2's bin"
    output=$(TEST_SHELL="/bin/$2" run_installer)
    printf '%s\n' "$output" | grep -F "$3" >/dev/null || fail 'PATH hint must name the profile read by the user shell'
    if [ "$2" = fish ]; then
        printf '%s\n' "$output" | grep -F "fish_add_path -- '" >/dev/null || fail 'fish needs fish_add_path'
        printf '%s\n' "$output" | grep -F "fish\\'s bin" >/dev/null || fail 'fish must escape apostrophes'
    fi
    printf 'PASS: %s %s profile hint\n' "$os" "$2"
done
os=Linux arch=x86_64 destination="$fixture/wsl"
output=$(TEST_KERNEL=microsoft-standard-WSL2 run_installer)
printf '%s\n' "$output" | grep -F 'WSL detected' >/dev/null || fail 'WSL must explain the Linux binary selection'
# Uppercase checksums are valid.
# Uppercase only the digest, never the case-sensitive filename.
awk '{ print toupper($1) "  " $2 }' "$fixture/pristine-sums" > "$release/SHA256SUMS"
run_installer > /dev/null
printf 'PASS: uppercase checksums accepted\n'
expect_rejection() {
    destination="$fixture/rejected-$1"
    if run_installer > "$fixture/failure.log" 2>&1; then fail "$1 must fail closed"; fi
    [ ! -e "$destination" ] || fail "$1 must not create an installation"
    printf 'PASS: %s rejected before installation\n' "$1"
}
cp "$fixture/pristine-sums" "$release/SHA256SUMS"
cat "$fixture/pristine-sums" >> "$release/SHA256SUMS"
expect_rejection duplicate-checksum
rm "$release/SHA256SUMS"
expect_rejection missing-manifest
cp "$fixture/pristine-sums" "$release/SHA256SUMS"
version='1.2.3/../../bad'
expect_rejection invalid-version
version=$release_version
os=FreeBSD
expect_rejection unsupported-os
os=Linux arch=mips
expect_rejection unsupported-architecture
arch=x86_64 version=9.9.9
expect_rejection download-failure
version=$release_version
asset="kaisa-$release_version-x86_64-unknown-linux-musl.tar.gz"
rm "$fixture/kaisa"
ln -s /bin/sh "$fixture/kaisa"
tar -czf "$release/$asset" -C "$fixture" kaisa
digest=$(checksum "$release/$asset")
printf '%s  %s\n' "${digest%% *}" "$asset" > "$release/SHA256SUMS"
expect_rejection symlink-binary

# A failing curl spy proves the production request is HTTPS-only without any network.
cat > "$fixture/tools/curl" <<'CURL'
#!/bin/sh
printf '%s\n' "$@" > "$TEST_CURL_ARGS"
exit 22
CURL
chmod +x "$fixture/tools/curl"
destination="$fixture/https-rejected"
if TEST_CURL_ARGS="$fixture/curl-args" HOME="$fixture/home" PATH="$fixture/tools:$PATH" \
    TEST_OS=Linux TEST_ARCH=x86_64 KAISA_INSTALL_BASE_URL=https://example.invalid/releases \
    KAISA_INSTALL_DIR="$destination" KAISA_VERSION="$release_version" sh "$installer" > "$fixture/https.log" 2>&1; then
    fail 'download failure must stop installation'
fi
[ ! -e "$destination" ] || fail 'download failure must not create a destination'
for flag in --proto --proto-redir --tlsv1.2; do
    grep -Fx -- "$flag" "$fixture/curl-args" >/dev/null || fail "HTTPS request must include $flag"
done
[ "$(grep -Fxc '=https' "$fixture/curl-args")" -eq 2 ] || fail 'initial and redirected protocols must both restrict HTTPS'
printf 'PASS: HTTPS downloads restrict initial and redirected protocols and fail closed\n'
