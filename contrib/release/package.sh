#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 3 || $# -gt 4 ]]; then
    echo 'Usage: package.sh BINARY TARGET OUTPUT_DIRECTORY [RUST_TOOLCHAIN]' >&2
    exit 2
fi

binary=$1
target=$2
output=$3
toolchain=${4:-stable}

case "$target" in
    aarch64-unknown-linux-musl | x86_64-unknown-linux-musl) ;;
    *) echo 'Expected an ARM64 or x86-64 Linux musl target.' >&2; exit 2 ;;
esac

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
metadata=$(cargo "+$toolchain" metadata --locked --format-version 1 \
    --manifest-path "$root/Cargo.toml" --filter-platform "$target")
version=$(jq -er '.packages[] | select(.source == null and .name == "zest") | .version' <<< "$metadata")
archive="zest-v$version-$target.tar.gz"
stage=$(mktemp -d)
trap 'rm -rf -- "$stage"' EXIT

install -m 0755 "$binary" "$stage/zest"
install -m 0644 "$root/README.md" "$root/LICENSE" "$root/THIRD-PARTY-NOTICES.md" "$stage/"
mkdir -p "$stage/assets" "$stage/contrib/systemd" "$stage/licenses/rust"
install -m 0644 "$root/assets/HTMX-LICENSE.txt" "$stage/assets/"
install -m 0644 "$root/contrib/systemd/zest.service" \
    "$root/contrib/systemd/zest.env.example" "$stage/contrib/systemd/"

jq -r '.packages[] | select(.source != null) |
    [.name, .version, .manifest_path] | @tsv' <<< "$metadata" |
while IFS=$'\t' read -r name version manifest; do
    source_dir=${manifest%/*}
    destination="$stage/licenses/$name-$version"
    case "$name-$version" in
        maud-0.27.0 | maud_macros-0.27.0)
            source_dir="$root/contrib/release/licenses/maud-0.27.0"
            ;;
    esac

    shopt -s nullglob nocaseglob
    files=("$source_dir"/license* "$source_dir"/copying* "$source_dir"/copyright* "$source_dir"/notice*)
    if [[ ${#files[@]} -eq 0 ]]; then
        echo "Missing upstream notices for $name $version." >&2
        exit 1
    fi
    mkdir -p "$destination"
    for file in "${files[@]}"; do
        cp -R -- "$file" "$destination/"
    done
done

rust_docs="$(rustc "+$toolchain" --print sysroot)/share/doc/rust"
if [[ ! -f "$rust_docs/COPYRIGHT-library.html" ]]; then
    echo "Install the rust-docs component for toolchain $toolchain to package its notices." >&2
    exit 1
fi
install -m 0644 "$rust_docs/COPYRIGHT-library.html" "$stage/licenses/rust/"
cp -R -- "$rust_docs/licenses" "$stage/licenses/rust/"
cp -R -- "$root/contrib/release/licenses/musl-1.2.5" "$stage/licenses/"
rustc "+$toolchain" --version --verbose > "$stage/licenses/rust/VERSION"

mkdir -p -- "$output"
COPYFILE_DISABLE=1 tar -czf "$output/$archive" -C "$stage" \
    zest README.md LICENSE THIRD-PARTY-NOTICES.md assets contrib licenses
cd -- "$output"
sha256sum "$archive" > SHA256SUMS
