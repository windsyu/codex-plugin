#!/bin/sh
# Install this extracted release locally. No downloads or shell configuration.
set -eu
fail() { printf '%s\n' "install: $*" >&2; exit 1; }
prefix=${HOME:?HOME must be set}/.local
force=false
while [ "$#" -gt 0 ]; do
  case "$1" in
    --prefix) [ "$#" -ge 2 ] || fail '--prefix needs a path'; prefix=$2; shift 2 ;;
    --force) force=true; shift ;;
    -h|--help) printf '%s\n' 'Usage: sh install.sh [--prefix ABSOLUTE_PATH] [--force]'; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done
[ "$(uname -s)" = Darwin ] && [ "$(uname -m)" = arm64 ] || fail 'this release requires macOS on Apple Silicon (Darwin arm64)'
case "$prefix" in /*) ;; *) fail '--prefix must be absolute' ;; esac
case "$prefix" in
  /|*/../*|*/..|*/./*|*/.) fail 'prefix must not be root or contain dot path components' ;;
esac
release_dir=$(CDPATH= cd -P -- "$(dirname -- "$0")" && pwd)
resources='LICENSE THIRD_PARTY_NOTICES.md RELEASE.md manifest.json Cargo.lock web/package-lock.json CHANGELOG.md vendor/vt100/LICENSE vendor/vt100/PATCH.md web/src/workbench/icons/LICENSE web/src/workbench/icons/README.md'
# Check every existing ancestor before mkdir/cp can follow it.
check_ancestors() {
  check_path=$1
  while [ "$check_path" != / ]; do
    [ ! -L "$check_path" ] || fail "symbolic link destination: $check_path"
    if [ -e "$check_path" ]; then [ -d "$check_path" ] || fail "not a directory: $check_path"; fi
    check_path=$(dirname -- "$check_path")
  done
}
check_ancestors "$prefix"
check_ancestors "$prefix/bin"
check_ancestors "$prefix/share/code-view"
for name in code-view codex-view codex-observerd; do
  source_file=$release_dir/bin/$name
  [ -f "$source_file" ] && [ ! -L "$source_file" ] && [ -x "$source_file" ] || fail "missing executable: $name"
done
for name in $resources CHECKSUMS.sha256; do
  check_ancestors "$(dirname -- "$release_dir/$name")"
  [ -f "$release_dir/$name" ] && [ ! -L "$release_dir/$name" ] || fail "missing release file: $name"
done
[ -d "$release_dir/licenses" ] && [ ! -L "$release_dir/licenses" ] || fail 'missing licenses directory'
[ -z "$(find "$release_dir/licenses" -type l -print)" ] || fail 'symbolic link in release licenses'
(cd "$release_dir" && shasum -a 256 -c --status CHECKSUMS.sha256) || fail 'release checksum verification failed'
# An existing installation may contain extras; force preserves them, but never
# follows links. Reject all conflicts before creating even one destination file.
if [ -d "$prefix/share/code-view" ]; then
  [ -z "$(find "$prefix/share/code-view" -type l -print)" ] || fail 'symbolic link in installed resources'
  [ "$force" = true ] || fail 'share/code-view already exists; use --force to reinstall'
fi
for name in code-view codex-view codex-observerd; do
  destination=$prefix/bin/$name
  [ ! -L "$destination" ] || fail "symbolic link destination: $destination"
  if [ -e "$destination" ]; then
    [ -f "$destination" ] || fail "not a file: $destination"
    [ "$force" = true ] || fail "$destination already exists; use --force to reinstall"
  fi
done
for name in $resources; do
  destination=$prefix/share/code-view/$name
  check_ancestors "$(dirname -- "$destination")"
  [ ! -L "$destination" ] || fail "symbolic link destination: $destination"
  if [ -e "$destination" ]; then [ -f "$destination" ] || fail "not a file: $destination"; fi
done
if [ -e "$prefix/share/code-view/licenses" ]; then
  [ -d "$prefix/share/code-view/licenses" ] || fail 'installed licenses is not a directory'
fi
# Validate recursive directory/file type conflicts as part of the preflight.
find "$release_dir/licenses" -type d -exec sh -c '
  root=$1; target=$2; shift 2
  for source_directory do
    relative=${source_directory#"$root"}
    destination=$target$relative
    if [ -e "$destination" ] && [ ! -d "$destination" ]; then exit 1; fi
  done
' sh "$release_dir/licenses" "$prefix/share/code-view/licenses" {} + || fail 'installed license directory type conflict'
find "$release_dir/licenses" -type f -exec sh -c '
  root=$1; target=$2; shift 2
  for source_file do
    relative=${source_file#"$root"/}
    destination=$target/$relative
    if [ -e "$destination" ] && [ ! -f "$destination" ]; then exit 1; fi
    directory=$(dirname -- "$destination")
    while [ "$directory" != "$target" ]; do
      if [ -e "$directory" ] && [ ! -d "$directory" ]; then exit 1; fi
      directory=$(dirname -- "$directory")
    done
  done
' sh "$release_dir/licenses" "$prefix/share/code-view/licenses" {} + || fail 'installed license path type conflict'
mkdir -p "$prefix/bin" "$prefix/share/code-view"
for name in code-view codex-view codex-observerd; do
  cp "$release_dir/bin/$name" "$prefix/bin/$name"
  chmod 755 "$prefix/bin/$name"
done
for name in $resources; do
  mkdir -p "$(dirname -- "$prefix/share/code-view/$name")"
  cp "$release_dir/$name" "$prefix/share/code-view/$name"
done
cp -R "$release_dir/licenses" "$prefix/share/code-view/"
printf '%s\n' "Installed code-view in $prefix/bin" "Add that directory to PATH if needed; shell configuration was not changed."
